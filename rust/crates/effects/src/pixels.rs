//! Pixel work Desk mode and the name tag share: a Gaussian blur as OpenCV makes it, and frames as plain RGB.

use telescope_core::frame::{Frame, PixelFormat};

/// cv2.GaussianBlur(src, (0, 0), sigma) on one 8-bit channel: the kernel OpenCV picks for it, edges reflected.
pub fn gaussian_blur(src: &[u8], w: usize, h: usize, sigma: f64) -> Vec<u8> {
    let size = ((sigma * 3.0 * 2.0 + 1.0).round() as usize) | 1;
    let half = (size / 2) as i64;
    let mut kernel: Vec<f32> = (-half..=half).map(|i| (-(i * i) as f64 / (2.0 * sigma * sigma)).exp() as f32).collect();
    let total: f32 = kernel.iter().sum();
    kernel.iter_mut().for_each(|k| *k /= total);
    let reflect = |i: i64, n: i64| {
        let mut i = i;
        if n == 1 {
            return 0;
        }
        loop {
            if i < 0 {
                i = -i;
            } else if i >= n {
                i = 2 * (n - 1) - i;
            } else {
                return i as usize;
            }
        }
    };
    let mut mid = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            mid[y * w + x] = kernel.iter().enumerate().map(|(k, c)| c * src[y * w + reflect(x as i64 + k as i64 - half, w as i64)] as f32).sum();
        }
    }
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            let v: f32 = kernel.iter().enumerate().map(|(k, c)| c * mid[reflect(y as i64 + k as i64 - half, h as i64) * w + x]).sum();
            out[y * w + x] = v.round().clamp(0.0, 255.0) as u8;
        }
    }
    out
}

/// The frame as 3 bytes a pixel (R, G, B). I420 is BT.601 video range with each chroma sample shared by its 2 x 2 pixels, as
/// OpenCV's YUV conversions read it. None for a format that isn't one of the picture formats.
pub fn to_rgb(frame: &Frame) -> Option<Vec<u8>> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let mut out = Vec::with_capacity(w * h * 3);
    match frame.format {
        PixelFormat::Rgba => frame.data.chunks_exact(4).for_each(|p| out.extend_from_slice(&p[..3])),
        PixelFormat::Bgra => frame.data.chunks_exact(4).for_each(|p| out.extend([p[2], p[1], p[0]])),
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

/// Puts RGB pixels (3 bytes each, the frame's size) back into the frame in its own format.
pub fn put_rgb(frame: &mut Frame, rgb: &[u8]) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let format = frame.format;
    let data = frame.data_mut();
    match format {
        PixelFormat::Rgba => data.chunks_exact_mut(4).zip(rgb.chunks_exact(3)).for_each(|(d, p)| d[..3].copy_from_slice(p)),
        PixelFormat::Bgra => data.chunks_exact_mut(4).zip(rgb.chunks_exact(3)).for_each(|(d, p)| d[..3].copy_from_slice(&[p[2], p[1], p[0]])),
        PixelFormat::I420 => {
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let (luma, chroma) = data.split_at_mut(w * h);
            let (us, vs) = chroma.split_at_mut(cw * ch);
            let px = |x: usize, y: usize| (rgb[(y * w + x) * 3] as f32, rgb[(y * w + x) * 3 + 1] as f32, rgb[(y * w + x) * 3 + 2] as f32);
            for y in 0..h {
                for x in 0..w {
                    let (r, g, b) = px(x, y);
                    luma[y * w + x] = (16.0 + 0.257 * r + 0.504 * g + 0.098 * b).round().clamp(0.0, 255.0) as u8;
                }
            }
            for cy in 0..ch {
                for cx in 0..cw {
                    let (mut su, mut sv, mut n) = (0.0, 0.0, 0.0);
                    for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                        let (x, y) = (cx * 2 + dx, cy * 2 + dy);
                        if x < w && y < h {
                            let (r, g, b) = px(x, y);
                            su += 128.0 - 0.148 * r - 0.291 * g + 0.439 * b;
                            sv += 128.0 + 0.439 * r - 0.368 * g - 0.071 * b;
                            n += 1.0;
                        }
                    }
                    us[cy * cw + cx] = (su / n).round().clamp(0.0, 255.0) as u8;
                    vs[cy * cw + cx] = (sv / n).round().clamp(0.0, 255.0) as u8;
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn luma_below_black_counts_as_black_like_opencv() {
        // cv2.cvtColor(..., COLOR_YUV2RGB_I420) gives [203, 0, 0] for Y=0, U=128, V=255
        let frame = Frame::new(2, 2, PixelFormat::I420, vec![0, 0, 0, 0, 128, 255]);
        assert_eq!(&to_rgb(&frame).unwrap()[..3], &[203, 0, 0]);
    }

    #[test]
    fn a_picture_survives_a_trip_through_i420() {
        let (w, h) = (8, 6);
        let rgb: Vec<u8> = (0..w * h).flat_map(|i| [(i * 5) as u8, 90, (200 - i * 3) as u8]).collect();
        let mut frame = Frame::new(w as u32, h as u32, PixelFormat::I420, vec![0; PixelFormat::I420.size(w as u32, h as u32)]);
        put_rgb(&mut frame, &rgb);
        let back = to_rgb(&frame).unwrap();
        let worst = rgb.iter().zip(&back).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
        // chroma is shared by 2 x 2 pixels, so a ramp comes back a little off
        assert!(worst < 40, "{worst}");
        let grey = Frame::new(2, 2, PixelFormat::Rgba, [120, 120, 120, 255].repeat(4));
        let mut yuv = Frame::new(2, 2, PixelFormat::I420, vec![0; 6]);
        put_rgb(&mut yuv, &to_rgb(&grey).unwrap());
        assert!(to_rgb(&yuv).unwrap().iter().all(|v| (*v as i32 - 120).abs() <= 1));
    }
}
