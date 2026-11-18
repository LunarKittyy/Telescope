//! A stream's picture as the packed pixels a sender shares: B, G, R, A per pixel, top row first. The alpha is a separate argument so the cut-out can fill it later.

use crate::convert::i420_to_bgra_into;
use telescope_core::frame::{Frame, PixelFormat};

/// The frame as BGRA into `out`, with `alpha` (one byte a pixel, the frame's size) in the fourth byte when given and opaque otherwise. False for a format this can't make.
pub fn bgra(frame: &Frame, alpha: Option<&[u8]>, out: &mut Vec<u8>) -> bool {
    let pixels = frame.width as usize * frame.height as usize;
    match frame.format {
        PixelFormat::I420 => i420_to_bgra_into(frame, out),
        PixelFormat::Bgra => {
            out.clear();
            out.extend_from_slice(&frame.data[..(pixels * 4).min(frame.data.len())]);
            out.resize(pixels * 4, 0);
        }
        PixelFormat::Rgba => {
            out.clear();
            out.extend_from_slice(&frame.data[..(pixels * 4).min(frame.data.len())]);
            out.resize(pixels * 4, 0);
            out.chunks_exact_mut(4).for_each(|px| px.swap(0, 2));
        }
        _ => return false,
    }
    match alpha {
        Some(mask) if mask.len() == pixels => out.chunks_exact_mut(4).zip(mask).for_each(|(px, a)| px[3] = *a),
        _ => out.chunks_exact_mut(4).for_each(|px| px[3] = 255),
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(format: PixelFormat, data: Vec<u8>) -> Frame {
        Frame::new(2, 2, format, data)
    }

    #[test]
    fn an_rgba_picture_swaps_red_and_blue_and_stays_opaque_without_a_mask() {
        let f = frame(PixelFormat::Rgba, [10u8, 20, 30, 0].repeat(4));
        let mut out = Vec::new();
        assert!(bgra(&f, None, &mut out));
        assert_eq!(out, [30u8, 20, 10, 255].repeat(4));
    }

    #[test]
    fn a_bgra_picture_keeps_its_colours_and_takes_the_mask_as_alpha() {
        let f = frame(PixelFormat::Bgra, [1u8, 2, 3, 255].repeat(4));
        let mut out = Vec::new();
        assert!(bgra(&f, Some(&[0, 64, 128, 255]), &mut out));
        assert_eq!(out, [1, 2, 3, 0, 1, 2, 3, 64, 1, 2, 3, 128, 1, 2, 3, 255]);
        // A mask of the wrong size is ignored, not read past its end
        assert!(bgra(&f, Some(&[0, 0]), &mut out));
        assert!(out.chunks_exact(4).all(|px| px[3] == 255));
    }

    #[test]
    fn an_i420_picture_comes_out_top_row_first_in_blue_green_red_order() {
        // Full-range red-ish: Y 81, U 90, V 240 is red in limited range
        let mut data = vec![81u8; 4];
        data.extend([90, 240]);
        let f = frame(PixelFormat::I420, data);
        let mut out = Vec::new();
        assert!(bgra(&f, None, &mut out));
        assert_eq!(out.len(), 16);
        for px in out.chunks_exact(4) {
            assert!(px[2] > 200 && px[0] < 60 && px[3] == 255, "{px:?}");
        }
    }

    #[test]
    fn a_format_it_cant_make_is_refused() {
        let f = frame(PixelFormat::Nv12, vec![0; 6]);
        assert!(!bgra(&f, None, &mut Vec::new()));
    }
}
