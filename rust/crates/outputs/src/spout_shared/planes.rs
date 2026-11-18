//! The plane rules of an I420 picture and the row packing of a mapped texture, plain arithmetic kept apart from Direct3D so every system checks them.

/// The size of each chroma plane of a width x height picture: odd sizes round up, as the CPU's conversion does.
pub fn chroma_size(width: u32, height: u32) -> (u32, u32) {
    (width.div_ceil(2), height.div_ceil(2))
}

/// The bytes of an I420 picture: the luma plane, then the U plane, then the V plane, each packed with no padding.
pub fn i420_len(width: u32, height: u32) -> usize {
    let (cw, ch) = chroma_size(width, height);
    width as usize * height as usize + 2 * cw as usize * ch as usize
}

/// A sender's DXGI format number as the converter takes it: the typeless format of its own copy and the UNORM one the shader reads it as (sRGB is read as plain bytes, as the CPU does).
pub fn card_formats(format: u32) -> Option<(u32, u32)> {
    match format {
        87 | 90 | 91 => Some((90, 87)), // B8G8R8A8
        88 | 92 | 93 => Some((92, 88)), // B8G8R8X8
        27..=29 => Some((27, 28)),      // R8G8B8A8
        _ => None,
    }
}

/// How many bytes of a mapped texture its `height` rows take when they are `pitch` bytes apart and `row` bytes are wanted of each (the last row has no padding after it).
pub fn mapped_len(pitch: usize, row: usize, height: usize) -> usize {
    if height == 0 {
        0
    } else {
        pitch * (height - 1) + row
    }
}

/// Appends the `height` rows of `row` bytes `pitch` bytes apart in `src` to `out`, in one copy when there's no padding; None (and `out` as it was) for a short `src` or a `pitch` under `row`.
pub fn pack_rows(src: &[u8], pitch: usize, row: usize, height: usize, out: &mut Vec<u8>) -> Option<()> {
    if height == 0 || row == 0 {
        return Some(());
    }
    if pitch < row || src.len() < mapped_len(pitch, row, height) {
        return None;
    }
    out.reserve(row * height);
    if pitch == row {
        out.extend_from_slice(&src[..row * height]);
    } else {
        for y in 0..height {
            out.extend_from_slice(&src[y * pitch..][..row]);
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use telescope_core::frame::PixelFormat;

    #[test]
    fn odd_sizes_round_the_chroma_up_like_the_cpu() {
        assert_eq!(chroma_size(1920, 1080), (960, 540));
        assert_eq!(chroma_size(1, 1), (1, 1));
        assert_eq!(chroma_size(5, 3), (3, 2));
        assert_eq!(chroma_size(4, 7), (2, 4));
    }

    #[test]
    fn every_format_a_sender_may_have_but_the_unknown_one_goes_to_the_card() {
        use super::super::protocol::ChannelOrder;
        for format in 1..200 {
            let offered = card_formats(format);
            assert_eq!(offered.is_some(), ChannelOrder::of_dxgi(format).is_some(), "format {format}");
        }
        assert_eq!(card_formats(91), Some((90, 87)));
        assert_eq!(card_formats(93), Some((92, 88)));
        assert_eq!(card_formats(28), Some((27, 28)));
        assert_eq!(card_formats(0), None);
    }

    #[test]
    fn the_length_is_what_the_frame_format_says() {
        for (w, h) in [(1, 1), (2, 2), (3, 5), (17, 9), (1920, 1080), (1919, 1079)] {
            assert_eq!(i420_len(w, h), PixelFormat::I420.size(w, h), "{w}x{h}");
        }
    }

    #[test]
    fn padded_rows_are_packed_without_the_padding() {
        // 3 rows of 5 bytes, 8 apart
        let src: Vec<u8> = (0..21).collect();
        let mut out = vec![99];
        assert_eq!(pack_rows(&src, 8, 5, 3, &mut out), Some(()));
        assert_eq!(out, [99, 0, 1, 2, 3, 4, 8, 9, 10, 11, 12, 16, 17, 18, 19, 20]);
    }

    #[test]
    fn rows_with_no_padding_are_one_copy() {
        let src: Vec<u8> = (0..12).collect();
        let mut out = Vec::new();
        assert_eq!(pack_rows(&src, 4, 4, 3, &mut out), Some(()));
        assert_eq!(out, src);
    }

    #[test]
    fn the_last_row_needs_no_padding_after_it() {
        assert_eq!(mapped_len(8, 5, 3), 21);
        assert_eq!(mapped_len(8, 5, 0), 0);
        let src = vec![7u8; 21];
        assert_eq!(pack_rows(&src, 8, 5, 3, &mut Vec::new()), Some(()));
        assert_eq!(pack_rows(&src[..20], 8, 5, 3, &mut Vec::new()), None);
    }

    #[test]
    fn a_pitch_shorter_than_the_row_or_a_short_source_is_refused_and_leaves_out_alone() {
        let mut out = vec![1, 2];
        assert_eq!(pack_rows(&[0; 100], 3, 5, 2, &mut out), None);
        assert_eq!(pack_rows(&[0; 4], 5, 5, 1, &mut out), None);
        assert_eq!(out, [1, 2]);
    }

    #[test]
    fn a_one_pixel_wide_plane_packs() {
        let src = [10u8, 0, 0, 0, 11, 0, 0, 0, 12];
        let mut out = Vec::new();
        assert_eq!(pack_rows(&src, 4, 1, 3, &mut out), Some(()));
        assert_eq!(out, [10, 11, 12]);
    }
}
