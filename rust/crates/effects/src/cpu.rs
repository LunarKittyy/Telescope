//! What the effects that can't be a shader share: pictures as plain RGB bytes to work on, and back into whatever format the frame
//! came in. The colours are the ones media's shaders use (BT.601, video range for YUV), so a CPU step and a shader step see and
//! leave the same picture.

use telescope_core::frame::{Frame, PixelFormat};

/// A picture as 8-bit R, G, B, row after row.
#[derive(Clone, Debug, PartialEq)]
pub struct Rgb {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

fn clamp8(v: f32) -> u8 {
    v.round().clamp(0.0, 255.0) as u8
}

fn from_yuv(y: u8, u: u8, v: u8) -> [u8; 3] {
    let c = 1.164383 * (y as f32 - 16.0);
    let (d, e) = (u as f32 - 128.0, v as f32 - 128.0);
    [clamp8(c + 1.596027 * e), clamp8(c - 0.391762 * d - 0.812968 * e), clamp8(c + 2.017232 * d)]
}

fn to_y(r: f32, g: f32, b: f32) -> f32 {
    16.0 + 0.256788 * r + 0.504129 * g + 0.097906 * b
}

fn to_u(r: f32, g: f32, b: f32) -> f32 {
    128.0 - 0.148223 * r - 0.290993 * g + 0.439216 * b
}

fn to_v(r: f32, g: f32, b: f32) -> f32 {
    128.0 + 0.439216 * r - 0.367788 * g - 0.071427 * b
}

impl Rgb {
    pub fn new(w: usize, h: usize) -> Rgb {
        Rgb { w, h, data: vec![0; w * h * 3] }
    }

    pub fn filled(w: usize, h: usize, rgb: [u8; 3]) -> Rgb {
        Rgb { w, h, data: rgb.iter().copied().cycle().take(w * h * 3).collect() }
    }

    pub fn at(&self, x: usize, y: usize) -> [u8; 3] {
        let i = (y * self.w + x) * 3;
        [self.data[i], self.data[i + 1], self.data[i + 2]]
    }

    pub fn set(&mut self, x: usize, y: usize, rgb: [u8; 3]) {
        let i = (y * self.w + x) * 3;
        self.data[i..i + 3].copy_from_slice(&rgb);
    }

    /// The picture a frame holds; None for a format this doesn't know.
    pub fn from_frame(frame: &Frame) -> Option<Rgb> {
        let (w, h) = (frame.width as usize, frame.height as usize);
        let mut out = Rgb::new(w, h);
        let d = &frame.data;
        match frame.format {
            PixelFormat::Rgba | PixelFormat::Bgra => {
                let (r, b) = if frame.format == PixelFormat::Rgba { (0, 2) } else { (2, 0) };
                for (o, p) in out.data.chunks_exact_mut(3).zip(d.chunks_exact(4)) {
                    o.copy_from_slice(&[p[r], p[1], p[b]]);
                }
            }
            PixelFormat::I420 | PixelFormat::Nv12 => {
                let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
                let (y_plane, rest) = d.split_at(w * h);
                for y in 0..h {
                    for x in 0..w {
                        let ci = (y / 2) * cw + x / 2;
                        let (u, v) = if frame.format == PixelFormat::I420 { (rest[ci], rest[cw * ch + ci]) } else { (rest[ci * 2], rest[ci * 2 + 1]) };
                        out.set(x, y, from_yuv(y_plane[y * w + x], u, v));
                    }
                }
            }
            _ => return None,
        }
        Some(out)
    }

    /// Puts the picture back into frame (the same size and format it was read from). YUV gets the average of each 2 x 2 for its chroma.
    pub fn write_to(&self, frame: &mut Frame) {
        let (w, h) = (self.w, self.h);
        debug_assert_eq!((w, h), (frame.width as usize, frame.height as usize));
        let format = frame.format;
        let d = frame.data_mut();
        match format {
            PixelFormat::Rgba | PixelFormat::Bgra => {
                let (r, b) = if format == PixelFormat::Rgba { (0, 2) } else { (2, 0) };
                for (p, o) in d.chunks_exact_mut(4).zip(self.data.chunks_exact(3)) {
                    p[r] = o[0];
                    p[1] = o[1];
                    p[b] = o[2];
                    p[3] = 255;
                }
            }
            PixelFormat::I420 | PixelFormat::Nv12 => {
                let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
                let (y_plane, rest) = d.split_at_mut(w * h);
                for (i, p) in self.data.chunks_exact(3).enumerate() {
                    y_plane[i] = clamp8(to_y(p[0] as f32, p[1] as f32, p[2] as f32));
                }
                for cy in 0..ch {
                    for cx in 0..cw {
                        let (mut u, mut v, mut n) = (0.0, 0.0, 0.0);
                        for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                            let (x, y) = ((cx * 2 + dx).min(w - 1), (cy * 2 + dy).min(h - 1));
                            let p = self.at(x, y);
                            let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
                            u += to_u(r, g, b);
                            v += to_v(r, g, b);
                            n += 1.0;
                        }
                        let (u, v) = (clamp8(u / n), clamp8(v / n));
                        let ci = cy * cw + cx;
                        if format == PixelFormat::I420 {
                            rest[ci] = u;
                            rest[cw * ch + ci] = v;
                        } else {
                            rest[ci * 2] = u;
                            rest[ci * 2 + 1] = v;
                        }
                    }
                }
            }
            _ => {}
        }
    }
}

/// One pixel of a frame as R, G, B; None outside it.
pub fn rgb_at(frame: &Frame, x: usize, y: usize) -> Option<[u8; 3]> {
    let (w, h) = (frame.width as usize, frame.height as usize);
    if x >= w || y >= h {
        return None;
    }
    let d = &frame.data;
    Some(match frame.format {
        PixelFormat::Rgba => {
            let i = (y * w + x) * 4;
            [d[i], d[i + 1], d[i + 2]]
        }
        PixelFormat::Bgra => {
            let i = (y * w + x) * 4;
            [d[i + 2], d[i + 1], d[i]]
        }
        PixelFormat::I420 | PixelFormat::Nv12 => {
            let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
            let ci = (y / 2) * cw + x / 2;
            let (u, v) = if frame.format == PixelFormat::I420 { (d[w * h + ci], d[w * h + cw * ch + ci]) } else { (d[w * h + ci * 2], d[w * h + ci * 2 + 1]) };
            from_yuv(d[y * w + x], u, v)
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(format: PixelFormat, w: u32, h: u32, f: impl Fn(&mut Rgb)) -> Frame {
        let mut img = Rgb::new(w as usize, h as usize);
        f(&mut img);
        let mut fr = Frame::new(w, h, format, vec![0; format.size(w, h)]);
        img.write_to(&mut fr);
        fr
    }

    #[test]
    fn rgba_and_bgra_keep_their_colours() {
        for format in [PixelFormat::Rgba, PixelFormat::Bgra] {
            let fr = frame(format, 3, 2, |img| img.set(1, 1, [10, 20, 30]));
            assert_eq!(Rgb::from_frame(&fr).unwrap().at(1, 1), [10, 20, 30]);
            assert_eq!(rgb_at(&fr, 1, 1), Some([10, 20, 30]));
            assert_eq!(rgb_at(&fr, 3, 0), None);
        }
    }

    #[test]
    fn a_flat_colour_survives_yuv_to_within_a_level() {
        for format in [PixelFormat::I420, PixelFormat::Nv12] {
            let fr = frame(format, 6, 4, |img| img.data.chunks_exact_mut(3).for_each(|p| p.copy_from_slice(&[200, 60, 90])));
            let back = Rgb::from_frame(&fr).unwrap().at(3, 2);
            assert!((0..3).all(|c| (back[c] as i32 - [200, 60, 90][c] as i32).abs() <= 2), "{back:?}");
            assert_eq!(rgb_at(&fr, 3, 2), Some(back));
        }
    }

    #[test]
    fn odd_sizes_work() {
        let fr = frame(PixelFormat::I420, 5, 3, |img| img.data.fill(128));
        assert_eq!(Rgb::from_frame(&fr).unwrap().at(4, 2)[0] / 4, 32);
    }
}
