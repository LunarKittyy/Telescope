//! The preview on the processor without FFmpeg, the steps Python's preview takes on every frame: a copy with no graphics card and
//! no FFmpeg still shows the picture.

use telescope_core::frame::Frame;
use telescope_core::image::{frame_rgb, resize_area, resize_linear_fixed};

/// preview._fit_within: frame as RGB at out_w x out_h, shrunk by a whole factor with INTER_AREA first and finished with INTER_LINEAR.
pub fn rgb(frame: &Frame, out_w: u32, out_h: u32) -> Vec<u8> {
    let (mut w, mut h, ow, oh) = (frame.width as usize, frame.height as usize, out_w as usize, out_h as usize);
    let pixels = (w > 0 && h > 0 && ow > 0 && oh > 0).then(|| frame_rgb(frame)).flatten();
    let Some(mut pixels) = pixels else { return vec![0; ow * oh * 3] };
    let step = (w as f64 / ow as f64).max(h as f64 / oh as f64) as usize;
    if step >= 2 {
        pixels = resize_area(&pixels, w, h, 3, w / step, h / step);
        (w, h) = (w / step, h / step);
    }
    resize_linear_fixed(&pixels, w, h, 3, ow, oh)
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::frame::PixelFormat;

    // Recorded from preview._fit_within in OpenCV 4.13.0 on the picture gradient() draws
    const SHRUNK_9X6: &[u8] = &[11, 21, 125, 40, 21, 121, 69, 21, 127, 98, 21, 122, 127, 21, 136, 156, 21, 136, 186, 21, 114, 214, 21, 99, 243, 21, 141, 11, 61, 121, 40, 61, 122, 69, 61, 130, 98, 61, 129, 127, 61, 135, 156, 61, 121, 186, 61, 101, 214, 61, 132, 243, 61, 127, 11, 105, 117, 40, 105, 132, 69, 105, 129, 98, 105, 132, 127, 105, 125, 156, 105, 104, 186, 105, 124, 214, 105, 132, 243, 105, 111, 11, 149, 128, 40, 149, 134, 69, 149, 127, 98, 149, 127, 127, 149, 110, 156, 149, 115, 186, 149, 136, 214, 149, 116, 243, 149, 124, 11, 193, 143, 40, 193, 126, 69, 193, 125, 98, 193, 116, 127, 193, 109, 156, 193, 138, 186, 193, 120, 214, 193, 116, 243, 193, 148, 11, 233, 130, 40, 233, 118, 69, 233, 120, 98, 233, 105, 127, 233, 136, 156, 233, 127, 186, 233, 105, 214, 233, 151, 243, 233, 133];
    const SHRUNK_17X11: &[u8] = &[4, 6, 46, 19, 6, 132, 35, 6, 176, 50, 6, 91, 65, 6, 132, 81, 6, 137, 96, 6, 81, 112, 6, 142, 127, 6, 161, 142, 6, 114, 158, 6, 146, 173, 6, 128, 189, 6, 92, 204, 6, 150, 219, 6, 89, 235, 6, 79, 251, 6, 159, 4, 30, 130, 19, 30, 182, 35, 30, 89, 50, 30, 129, 65, 30, 142, 81, 30, 80, 96, 30, 138, 112, 30, 163, 127, 30, 117, 142, 30, 141, 158, 30, 132, 173, 30, 95, 189, 30, 144, 204, 30, 95, 219, 30, 80, 235, 30, 155, 251, 30, 163, 4, 54, 188, 19, 54, 86, 35, 54, 127, 50, 54, 148, 65, 54, 78, 81, 54, 135, 96, 54, 166, 112, 54, 118, 127, 54, 137, 142, 54, 136, 158, 54, 97, 173, 54, 138, 189, 54, 100, 204, 54, 80, 219, 54, 152, 235, 54, 159, 251, 54, 77, 4, 79, 83, 19, 79, 125, 35, 79, 153, 50, 79, 74, 65, 79, 131, 81, 79, 169, 96, 79, 121, 112, 79, 133, 127, 79, 139, 142, 79, 98, 158, 79, 134, 173, 79, 105, 189, 79, 82, 204, 79, 148, 219, 79, 155, 235, 79, 85, 251, 79, 153, 4, 103, 123, 19, 103, 159, 35, 103, 71, 50, 103, 128, 65, 103, 172, 81, 103, 121, 96, 103, 130, 112, 103, 142, 127, 103, 99, 142, 103, 130, 158, 103, 109, 173, 103, 82, 189, 103, 144, 204, 103, 153, 219, 103, 91, 235, 103, 150, 251, 103, 126, 4, 127, 165, 19, 127, 67, 35, 127, 125, 50, 127, 176, 65, 127, 120, 81, 127, 127, 96, 127, 146, 112, 127, 98, 127, 127, 127, 142, 127, 113, 158, 127, 82, 173, 127, 141, 189, 127, 152, 204, 127, 96, 219, 127, 147, 235, 127, 124, 251, 127, 70, 4, 151, 62, 19, 151, 122, 35, 151, 181, 50, 151, 119, 65, 151, 124, 81, 151, 150, 96, 151, 98, 112, 151, 124, 127, 151, 117, 142, 151, 82, 158, 151, 138, 173, 151, 151, 189, 151, 100, 204, 151, 144, 219, 151, 123, 235, 151, 75, 251, 151, 148, 4, 175, 119, 19, 175, 187, 35, 175, 117, 50, 175, 121, 65, 175, 156, 81, 175, 97, 96, 175, 122, 112, 175, 122, 127, 175, 81, 142, 175, 134, 158, 175, 151, 173, 175, 103, 189, 175, 141, 204, 175, 124, 219, 175, 79, 235, 175, 144, 251, 175, 189, 4, 200, 195, 19, 200, 113, 35, 200, 119, 50, 200, 162, 65, 200, 94, 81, 200, 119, 96, 200, 127, 112, 200, 78, 127, 200, 131, 142, 200, 154, 158, 200, 106, 173, 200, 137, 189, 200, 124, 204, 200, 82, 219, 200, 141, 235, 200, 186, 251, 200, 84, 4, 224, 109, 19, 224, 116, 35, 224, 169, 50, 224, 90, 65, 224, 117, 81, 224, 132, 96, 224, 76, 112, 224, 127, 127, 224, 156, 142, 224, 109, 158, 224, 134, 173, 224, 126, 189, 224, 84, 204, 224, 138, 219, 224, 183, 235, 224, 90, 251, 224, 142, 4, 248, 113, 19, 248, 178, 35, 248, 86, 50, 248, 115, 65, 248, 139, 81, 248, 72, 96, 248, 123, 112, 248, 158, 127, 248, 111, 142, 248, 130, 158, 248, 130, 173, 248, 86, 189, 248, 134, 204, 248, 181, 219, 248, 96, 235, 248, 140, 251, 248, 167];

    /// 37 x 23, a colour of its own in each pixel.
    fn gradient() -> Frame {
        let pixels = (0..23usize).flat_map(|y| (0..37usize).flat_map(move |x| [(x * 255 / 36) as u8, (y * 255 / 22) as u8, ((x + y) * 40 % 256) as u8, 255]));
        Frame::new(37, 23, PixelFormat::Rgba, pixels.collect())
    }

    #[test]
    fn it_shrinks_as_python_does_to_the_byte() {
        assert_eq!(rgb(&gradient(), 9, 6), SHRUNK_9X6, "a factor of 4, then the rest");
        assert_eq!(rgb(&gradient(), 17, 11), SHRUNK_17X11, "a factor of 2, then the rest");
    }

    #[test]
    fn bgra_comes_out_in_the_same_colours() {
        let source = gradient();
        let bgra = source.data.as_chunks::<4>().0.iter().flat_map(|p| [p[2], p[1], p[0], p[3]]).collect();
        assert_eq!(rgb(&Frame::new(37, 23, PixelFormat::Bgra, bgra), 9, 6), SHRUNK_9X6);
    }

    #[test]
    fn i420_comes_out_as_bt601_at_awkward_sizes() {
        // Y 180, U 100, V 160 is the salmon pink CpuScaler's test draws
        for (w, h, ow, oh) in [(638u32, 48u32, 638u32, 48u32), (637, 47, 637, 47), (33, 17, 33, 17), (638, 48, 321, 25), (99, 31, 203, 61)] {
            let c = (w.div_ceil(2) * h.div_ceil(2)) as usize;
            let frame = Frame::new(w, h, PixelFormat::I420, [vec![180; (w * h) as usize], vec![100; c], vec![160; c]].concat());
            let out = rgb(&frame, ow, oh);
            assert_eq!(out.len(), (ow * oh * 3) as usize);
            for px in out.as_chunks::<3>().0 {
                assert!(px.iter().zip([242, 175, 134]).all(|(&a, b)| (a as i32 - b).abs() <= 3), "{w}x{h} to {ow}x{oh} came out {px:?}");
            }
        }
    }

    #[test]
    fn nv12_comes_out_as_i420_does() {
        let (w, h) = (8usize, 4usize);
        let i420 = Frame::new(8, 4, PixelFormat::I420, [vec![180; w * h], vec![100; 8], vec![160; 8]].concat());
        let nv12 = Frame::new(8, 4, PixelFormat::Nv12, [vec![180; w * h], [100, 160].repeat(8)].concat());
        assert_eq!(rgb(&nv12, 4, 2), rgb(&i420, 4, 2));
    }

    #[test]
    fn nothing_to_draw_is_black_of_the_right_size() {
        assert_eq!(rgb(&Frame::new(0, 0, PixelFormat::Nv12, Vec::new()), 4, 2), vec![0; 24]);
        assert!(rgb(&gradient(), 0, 6).is_empty());
    }
}
