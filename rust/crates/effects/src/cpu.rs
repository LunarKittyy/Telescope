//! What the effects that can't be a shader share: pictures as plain RGB bytes to work on, and back into whatever format the frame
//! came in. The colours are the ones media's shaders use (BT.601, video range for YUV), so a CPU step and a shader step see and
//! leave the same picture.

use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::image::{byte as clamp8, keep_bytes, read_420, read_bgr, rgb_to_420, spare_bytes};

/// A picture as 8-bit R, G, B, row after row.
#[derive(Clone, Debug, PartialEq)]
pub struct Rgb {
    pub w: usize,
    pub h: usize,
    pub data: Vec<u8>,
}

fn from_yuv(y: u8, u: u8, v: u8) -> [u8; 3] {
    let c = 1.164383 * (y as f32 - 16.0);
    let (d, e) = (u as f32 - 128.0, v as f32 - 128.0);
    [clamp8(c + 1.596027 * e), clamp8(c - 0.391762 * d - 0.812968 * e), clamp8(c + 2.017232 * d)]
}

impl Drop for Rgb {
    fn drop(&mut self) {
        keep_bytes(std::mem::take(&mut self.data));
    }
}

impl Rgb {
    pub fn new(w: usize, h: usize) -> Rgb {
        let mut out = Rgb::scratch(w, h);
        out.data.fill(0);
        out
    }

    /// A picture to write every pixel of: until then what's in it is whatever a spare buffer had.
    pub fn scratch(w: usize, h: usize) -> Rgb {
        Rgb { w, h, data: spare_bytes(w * h * 3) }
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
        let mut out = Rgb::scratch(w, h);
        // The decoder's own BGR, as Python's effects get: the I420 has lost chroma on its way from there
        if read_bgr(frame, &mut out.data) {
            return Some(out);
        }
        let d = &frame.data;
        match frame.format {
            PixelFormat::Rgba | PixelFormat::Bgra => {
                let (r, b) = if frame.format == PixelFormat::Rgba { (0, 2) } else { (2, 0) };
                for (o, p) in out.data.as_chunks_mut::<3>().0.iter_mut().zip(d.as_chunks::<4>().0) {
                    o.copy_from_slice(&[p[r], p[1], p[b]]);
                }
            }
            PixelFormat::I420 | PixelFormat::Nv12 => read_420(frame, &mut out.data, false)?,
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
                for (p, o) in d.as_chunks_mut::<4>().0.iter_mut().zip(self.data.as_chunks::<3>().0) {
                    p[r] = o[0];
                    p[1] = o[1];
                    p[b] = o[2];
                    p[3] = 255;
                }
            }
            PixelFormat::I420 | PixelFormat::Nv12 => rgb_to_420(&self.data, w, h, d, format == PixelFormat::Nv12),
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
    if let Some(bgr) = frame.bgr() {
        let p = &bgr.row(y, w)[x * 3..];
        return Some([p[2], p[1], p[0]]);
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
    use std::sync::Arc;
    use telescope_core::frame::Bgr;

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
            let fr = frame(format, 6, 4, |img| img.data.as_chunks_mut::<3>().0.iter_mut().for_each(|p| p.copy_from_slice(&[200, 60, 90])));
            let back = Rgb::from_frame(&fr).unwrap().at(3, 2);
            assert!((0..3).all(|c| (back[c] as i32 - [200, 60, 90][c]).abs() <= 2), "{back:?}");
            assert_eq!(rgb_at(&fr, 3, 2), Some(back));
        }
    }

    #[test]
    fn effects_read_the_decoders_bgr_until_the_frame_changes() {
        let bgr = Bgr { data: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12], start: 0, stride: 6 };
        let mut fr = frame(PixelFormat::I420, 2, 2, |img| img.data.fill(128)).with_bgr(Arc::new(bgr));
        let seen = Rgb::from_frame(&fr).unwrap();
        assert_eq!(seen.data, [3, 2, 1, 6, 5, 4, 9, 8, 7, 12, 11, 10]);
        assert_eq!(rgb_at(&fr, 1, 1), Some([12, 11, 10]));
        seen.write_to(&mut fr);
        assert_eq!(fr.bgr(), None);
        assert_ne!(Rgb::from_frame(&fr).unwrap().data, seen.data, "read back through I420");
    }

    #[test]
    fn odd_sizes_work() {
        let fr = frame(PixelFormat::I420, 5, 3, |img| img.data.fill(128));
        assert_eq!(Rgb::from_frame(&fr).unwrap().at(4, 2)[0] / 4, 32);
    }
}
