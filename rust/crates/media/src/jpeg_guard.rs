//! Decoding a JPEG from a peer without trusting the size it declares: a few hundred bytes can claim a 3 GB frame.

pub const MAX_SIDE: u32 = 8192;
pub const MAX_PIXELS: u64 = 64_000_000;

// Start-of-frame markers: baseline, extended, progressive and lossless, Huffman or arithmetic coded (not C4, C8, CC)
fn is_sof(marker: u8) -> bool {
    matches!(marker, 0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF)
}

// TEM, SOI, EOI, RSTn
fn has_no_length(marker: u8) -> bool {
    matches!(marker, 0x01 | 0xD8 | 0xD9 | 0xD0..=0xD7)
}

fn be16(data: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([data[i], data[i + 1]])
}

/// (width, height) from the first start-of-frame header, or None if there isn't a readable one before the scan.
pub fn jpeg_size(data: &[u8]) -> Option<(u32, u32)> {
    let n = data.len();
    if n < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return None;
    }
    let mut i = 2;
    while i + 1 < n {
        if data[i] != 0xFF {
            return None;
        }
        // Fill bytes before a marker
        while i + 1 < n && data[i + 1] == 0xFF {
            i += 1;
        }
        if i + 1 >= n {
            return None;
        }
        let marker = data[i + 1];
        i += 2;
        if has_no_length(marker) {
            continue;
        }
        // The scan starts without a frame header before it, or the stream is garbage
        if marker == 0xDA || marker == 0x00 || i + 2 > n {
            return None;
        }
        let length = be16(data, i) as usize;
        if length < 2 {
            return None;
        }
        if is_sof(marker) {
            if length < 8 || i + 7 > n {
                return None;
            }
            let height = be16(data, i + 3) as u32;
            let width = be16(data, i + 5) as u32;
            return Some((width, height));
        }
        i += length;
    }
    None
}

/// Whether the JPEG declares a size worth decoding; one with no readable header isn't.
pub fn within_limits(data: &[u8]) -> bool {
    let Some((w, h)) = jpeg_size(data) else { return false };
    w > 0 && w <= MAX_SIDE && h > 0 && h <= MAX_SIDE && w as u64 * h as u64 <= MAX_PIXELS
}

/// What decode makes of the JPEG, which it only gets to see if the size it declares is worth decoding.
pub fn decode_checked<T>(data: &[u8], decode: impl FnOnce(&[u8]) -> Option<T>) -> Option<T> {
    if within_limits(data) { decode(data) } else { None }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::mjpeg::Mjpeg;

    /// A minimal baseline JPEG header: SOI, an APP0, a DQT, SOF0 for w x h, a DHT, then SOS.
    pub(crate) fn header(w: u16, h: u16, sof: u8) -> Vec<u8> {
        let mut j = vec![0xFF, 0xD8];
        j.extend([0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0, 1, 1, 0, 0, 1, 0, 1, 0, 0]);
        j.extend([0xFF, 0xDB, 0x00, 0x43, 0x00]);
        j.extend([1u8; 64]);
        j.extend([0xFF, sof, 0x00, 0x11, 0x08]);
        j.extend(h.to_be_bytes());
        j.extend(w.to_be_bytes());
        j.extend([3, 1, 0x22, 0, 2, 0x11, 0, 3, 0x11, 0]);
        j.extend([0xFF, 0xC4, 0x00, 0x14, 0x00]);
        j.extend([0u8; 17]);
        j.extend([0xFF, 0xDA, 0x00, 0x0C, 3, 1, 0, 2, 0x11, 3, 0x11, 0, 0x3F, 0]);
        j
    }

    fn patch_size(jpg: &[u8], w: u16, h: u16) -> Vec<u8> {
        let mut out = jpg.to_vec();
        let at = out.windows(2).position(|p| p == [0xFF, 0xC0]).unwrap() + 5;
        out[at..at + 2].copy_from_slice(&h.to_be_bytes());
        out[at + 2..at + 4].copy_from_slice(&w.to_be_bytes());
        out
    }

    // Made by Python's _jpeg with cv2.imencode
    const CV2_64X48: &[u8] = include_bytes!("../fixtures/jpeg/cv2_64x48.jpg");
    const CV2_33X7: &[u8] = include_bytes!("../fixtures/jpeg/cv2_33x7.jpg");
    const CV2_PROGRESSIVE: &[u8] = include_bytes!("../fixtures/jpeg/cv2_80x60_progressive.jpg");

    #[test]
    fn reads_the_size_of_real_encoder_output() {
        assert_eq!(jpeg_size(CV2_64X48), Some((64, 48)));
        assert_eq!(jpeg_size(CV2_33X7), Some((33, 7)));
        let decoded = Mjpeg::new().unwrap().decode_jpeg(CV2_33X7).expect("a real JPEG decodes");
        assert_eq!((decoded.width, decoded.height), (33, 7));
    }

    #[test]
    fn reads_a_progressive_jpeg() {
        assert!(CV2_PROGRESSIVE.windows(2).any(|p| p == [0xFF, 0xC2]));
        assert_eq!(jpeg_size(CV2_PROGRESSIVE), Some((80, 60)));
    }

    #[test]
    fn every_start_of_frame_variant_is_read() {
        for marker in [0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF] {
            assert_eq!(jpeg_size(&header(1234, 567, marker)), Some((1234, 567)), "{marker:x}");
        }
    }

    #[test]
    fn the_dht_marker_is_not_a_frame_header() {
        // A DHT (C4) before the frame header is skipped as a segment, not mistaken for one
        let mut j = header(64, 48, 0xC0);
        let dht = [0xFF, 0xC4, 0x00, 0x14, 0x00].iter().copied().chain([0u8; 17]).collect::<Vec<_>>();
        j.splice(2..2, dht);
        assert_eq!(jpeg_size(&j), Some((64, 48)));
    }

    #[test]
    fn segments_are_skipped_by_their_length_not_by_searching() {
        let jpg = header(20, 10, 0xC0);
        // A comment segment whose payload looks like a start-of-frame for 9999x9999 must not be believed
        let mut fake = vec![0xFF, 0xC0, 0x00, 0x11, 0x08, 0x27, 0x0F, 0x27, 0x0F, 0x03];
        fake.extend([0u8; 8]);
        let mut comment = vec![0xFF, 0xFE];
        comment.extend(((2 + fake.len()) as u16).to_be_bytes());
        comment.extend(fake);
        let data = [&jpg[..2], &comment, &jpg[2..]].concat();
        assert_eq!(jpeg_size(&data), Some((20, 10)));
    }

    #[test]
    fn fill_bytes_and_standalone_markers_are_tolerated() {
        let jpg = header(20, 10, 0xC0);
        assert_eq!(jpeg_size(&[&jpg[..2], &[0xFF, 0xFF, 0xFF][..], &jpg[2..]].concat()), Some((20, 10)));
        assert_eq!(jpeg_size(&[&jpg[..2], &[0xFF, 0x01][..], &jpg[2..]].concat()), Some((20, 10)));
    }

    #[test]
    fn exif_in_front_is_skipped() {
        let jpg = header(20, 10, 0xC0);
        let mut exif = vec![0xFF, 0xE1];
        exif.extend((2u16 + 300).to_be_bytes());
        exif.extend([0u8; 300]);
        assert_eq!(jpeg_size(&[&jpg[..2], &exif, &jpg[2..]].concat()), Some((20, 10)));
    }

    #[test]
    fn garbage_and_truncation_give_none_without_raising() {
        let cases: [&[u8]; 10] = [
            b"", b"\xff", b"\xff\xd8", b"not a jpeg at all", b"\xff\xd8\xff\xda\x00\x02", b"\xff\xd8\xff\xc0\x00",
            b"\xff\xd8\xff\xc0\x00\x0b\x08\x00", b"\xff\xd8\x00\x00", b"\xff\xd8\xff\xe0\x00\x01", b"\xff\xd8\xff\xe0\xff\xff\x00",
        ];
        let mut dec = Mjpeg::new().unwrap();
        for data in cases {
            assert_eq!(jpeg_size(data), None, "{data:?}");
            assert!(!within_limits(data));
            assert!(dec.decode_jpeg(data).is_none(), "{data:?}");
        }
    }

    #[test]
    fn a_truncated_real_jpeg_is_still_sized_when_its_header_is_whole() {
        let jpg = header(64, 48, 0xC0);
        let sos = jpg.windows(2).position(|p| p == [0xFF, 0xDA]).unwrap();
        let sof = jpg.windows(2).position(|p| p == [0xFF, 0xC0]).unwrap();
        assert_eq!(jpeg_size(&jpg[..sos + 2]), Some((64, 48)));
        assert_eq!(jpeg_size(&jpg[..sof + 4]), None);
    }

    #[test]
    fn limits() {
        let jpg = header(64, 48, 0xC0);
        for (w, h, ok) in [(8192, 4096, true), (8000, 8000, true), (8193, 10, false), (10, 8193, false),
                           (32767, 32767, false), (20000, 20000, false), (8192, 8192, false), (0, 100, false)] {
            assert_eq!(within_limits(&patch_size(&jpg, w, h)), ok, "{w}x{h}");
        }
    }
}
