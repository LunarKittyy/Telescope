//! Frames as plain RGB, for Desk mode.

use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::image::{rgb_to_420, spare_bytes};

pub use telescope_core::image::frame_rgb as to_rgb;

/// Puts RGB pixels (3 bytes each, the frame's size) back into the frame in its own format.
pub fn put_rgb(frame: &mut Frame, rgb: &[u8]) {
    let (w, h) = (frame.width as usize, frame.height as usize);
    let format = frame.format;
    if rgb.len() < w * h * 3 || !matches!(format, PixelFormat::Rgba | PixelFormat::Bgra | PixelFormat::I420 | PixelFormat::Nv12) {
        return;
    }
    // Written whole into a spare buffer that then replaces the pixels, so a panic leaves the frame as it was
    let mut out = spare_bytes(format.size(frame.width, frame.height));
    match format {
        // The alpha stays what the picture had
        PixelFormat::Rgba => out.as_chunks_mut::<4>().0.iter_mut().zip(rgb.as_chunks::<3>().0).zip(frame.data.as_chunks::<4>().0).for_each(|((d, p), old)| *d = [p[0], p[1], p[2], old[3]]),
        PixelFormat::Bgra => out.as_chunks_mut::<4>().0.iter_mut().zip(rgb.as_chunks::<3>().0).zip(frame.data.as_chunks::<4>().0).for_each(|((d, p), old)| *d = [p[2], p[1], p[0], old[3]]),
        _ => rgb_to_420(rgb, w, h, &mut out, format == PixelFormat::Nv12),
    }
    frame.replace_data(out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nv12_takes_a_picture_back_as_i420_does() {
        let (w, h) = (8, 6);
        let rgb: Vec<u8> = (0..w * h).flat_map(|i| [(i * 5) as u8, 90, (200 - i * 3) as u8]).collect();
        let mut nv12 = Frame::new(w as u32, h as u32, PixelFormat::Nv12, vec![0; PixelFormat::Nv12.size(w as u32, h as u32)]);
        let mut i420 = Frame::new(w as u32, h as u32, PixelFormat::I420, vec![0; PixelFormat::I420.size(w as u32, h as u32)]);
        put_rgb(&mut nv12, &rgb);
        put_rgb(&mut i420, &rgb);
        assert_eq!(to_rgb(&nv12).unwrap(), to_rgb(&i420).unwrap());
    }

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
