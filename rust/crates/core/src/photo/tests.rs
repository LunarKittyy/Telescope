use super::*;
use libjpeg_turbo_rs::{compress, decompress, PixelFormat, Subsampling};

fn jpeg(width: usize, height: usize, subsampling: Subsampling) -> Vec<u8> {
    let pixels: Vec<u8> = (0..height).flat_map(|y| (0..width).flat_map(move |x| {
        let mut bits = (x + y * width + 1) as u32;
        bits = (bits ^ (bits >> 16)).wrapping_mul(0x7feb352d);
        bits = (bits ^ (bits >> 15)).wrapping_mul(0x846ca68b);
        let value = ((bits ^ (bits >> 16)) % 220 + 16) as u8;
        [value, value, value]
    })).collect();
    compress(&pixels, width, height, PixelFormat::Rgb, 91, subsampling).unwrap()
}

fn exact_pixel_jpeg(width: usize, height: usize, subsampling: Subsampling) -> Vec<u8> {
    let pixels: Vec<u8> = (0..height).flat_map(|y| (0..width).flat_map(move |x| {
        let value = (((x / 8) * 29 + (y / 8) * 43) % 192 + 32) as u8;
        [value, value, value]
    })).collect();
    compress(&pixels, width, height, PixelFormat::Rgb, 91, subsampling).unwrap()
}

fn colour_jpeg(width: usize, height: usize, subsampling: Subsampling) -> Vec<u8> {
    let pixels: Vec<u8> = (0..width * height * 3).map(|i| {
        let mut bits = (i + 1) as u32;
        bits = (bits ^ (bits >> 16)).wrapping_mul(0x7feb352d);
        bits = (bits ^ (bits >> 15)).wrapping_mul(0x846ca68b);
        (bits ^ (bits >> 16)) as u8
    }).collect();
    compress(&pixels, width, height, PixelFormat::Rgb, 91, subsampling).unwrap()
}

fn reference(data: &[u8], t: &PhotoTransform) -> (usize, usize, Vec<u8>) {
    let image = decompress(data).unwrap();
    let (width, height) = if matches!(t.rotation, 90 | 270) { (image.height, image.width) } else { (image.width, image.height) };
    let mut output = vec![0; image.data.len()];
    for y in 0..image.height {
        for x in 0..image.width {
            let u = if t.flip_h { image.width - 1 - x } else { x };
            let v = if t.flip_v { image.height - 1 - y } else { y };
            let (dx, dy) = match t.rotation {
                90 => (image.height - 1 - v, u),
                180 => (image.width - 1 - u, image.height - 1 - v),
                270 => (v, image.width - 1 - u),
                _ => (u, v),
            };
            output[(dy * width + dx) * 3..(dy * width + dx + 1) * 3].copy_from_slice(&image.data[(y * image.width + x) * 3..(y * image.width + x + 1) * 3]);
        }
    }
    (width, height, output)
}

fn check_forward_coefficients(source: &[u8], output: &[u8], t: &PhotoTransform) {
    let before = libjpeg_turbo_rs::read_coefficients(source).unwrap();
    let after = libjpeg_turbo_rs::read_coefficients(output).unwrap();
    let swaps = matches!(t.rotation, 90 | 270);
    for (a, b) in before.components.iter().zip(&after.components) {
        assert_eq!((b.blocks_x, b.blocks_y), if swaps { (a.blocks_y, a.blocks_x) } else { (a.blocks_x, a.blocks_y) });
        for y in 0..a.blocks_y {
            for x in 0..a.blocks_x {
                let fx = if t.flip_h { a.blocks_x - 1 - x } else { x };
                let fy = if t.flip_v { a.blocks_y - 1 - y } else { y };
                let (dx, dy) = match t.rotation {
                    90 => (a.blocks_y - 1 - fy, fx),
                    180 => (a.blocks_x - 1 - fx, a.blocks_y - 1 - fy),
                    270 => (fy, a.blocks_x - 1 - fx),
                    _ => (fx, fy),
                };
                for v in 0..8 {
                    for u in 0..8 {
                        let (du, dv) = if swaps { (v, u) } else { (u, v) };
                        let rotation_odd = match t.rotation { 90 => v % 2, 180 => (u + v) % 2, 270 => u % 2, _ => 0 };
                        let odd = (usize::from(t.flip_h) * u + usize::from(t.flip_v) * v + rotation_odd) % 2;
                        let natural = libjpeg_turbo_rs::common::quant_table::NATURAL_ORDER;
                        let expected = a.blocks[y * a.blocks_x + x][natural[v * 8 + u]] * if odd == 1 { -1 } else { 1 };
                        assert_eq!(b.blocks[dy * b.blocks_x + dx][natural[dv * 8 + du]], expected);
                        assert_eq!(after.quant_tables[usize::from(b.quant_table_index)][dv * 8 + du], before.quant_tables[usize::from(a.quant_table_index)][v * 8 + u]);
                    }
                }
            }
        }
    }
}

#[test]
fn every_orientation_matches_pixels_and_round_trips() {
    for sampling in [Subsampling::S444, Subsampling::S420, Subsampling::S422] {
        // DC-only blocks give exact pixels; arbitrary AC blocks separately prove coefficient round trips despite integer IDCT rounding.
        let source = exact_pixel_jpeg(64, 48, sampling);
        let textured = jpeg(64, 48, sampling);
        let coloured = colour_jpeg(64, 48, sampling);
        for rotation in [0, 90, 180, 270] {
            for flip_h in [false, true] {
                for flip_v in [false, true] {
                    let t = PhotoTransform { rotation, flip_h, flip_v, ..Default::default() };
                    let output = apply_bytes(&source, &t).unwrap();
                    let image = decompress(&output).unwrap();
                    let expected = reference(&source, &t);
                    assert_eq!((image.width, image.height), (expected.0, expected.1));
                    assert!(image.data == expected.2, "rotation={rotation} h={flip_h} v={flip_v} sampling={sampling:?}, first differences: {:?}", image.data.iter().zip(&expected.2).enumerate().filter(|(_, (a, b))| a != b).take(12).collect::<Vec<_>>());
                    let inverse = PhotoTransform { rotation: if flip_h != flip_v { rotation } else { (360 - rotation) % 360 }, flip_h, flip_v, ..Default::default() };
                    for original in [&source, &textured, &coloured] {
                        let transformed = apply_bytes(original, &t).unwrap();
                        check_forward_coefficients(original, &transformed, &t);
                        let restored = apply_bytes(&transformed, &inverse).unwrap();
                        assert_eq!(decompress(&restored).unwrap().data, decompress(original).unwrap().data);
                        let before = libjpeg_turbo_rs::read_coefficients(original).unwrap();
                        let after = libjpeg_turbo_rs::read_coefficients(&restored).unwrap();
                        assert_eq!(before.quant_tables, after.quant_tables);
                        for (a, b) in before.components.iter().zip(&after.components) { assert_eq!(a.blocks, b.blocks); }
                    }
                }
            }
        }
    }
}

#[test]
fn optical_lens_share_is_removed_without_losing_manual_pan() {
    let t = PhotoTransform { zoom: 6.0, pan_x: 0.1, pan_y: -0.2, flip_h: true, ..Default::default() };
    let rest = t.for_lens(3.0);
    assert_eq!(rest.zoom, 2.0);
    assert!((rest.pan_x - 0.5).abs() < 1e-9);
    assert!((rest.pan_y + 1.0).abs() < 1e-9);
    assert!(rest.flip_h);
    assert_eq!(PhotoTransform::default().for_lens(3.0), PhotoTransform::default());
}

#[test]
fn crop_uses_whole_mcus_and_clamps_pan() {
    for (sampling, block) in [(Subsampling::S444, (8, 8)), (Subsampling::S420, (16, 16)), (Subsampling::S422, (16, 8))] {
        let source = jpeg(160, 96, sampling);
        for pan in [-1.0, -0.35, 0.0, 0.61, 1.0] {
            let t = PhotoTransform { zoom: 2.3, pan_x: pan, pan_y: -pan, ..Default::default() };
            let crop = crop_region(&source, &t).unwrap().unwrap();
            assert_eq!((crop.x % block.0, crop.width % block.0, crop.y % block.1, crop.height % block.1), (0, 0, 0, 0));
            let output = decompress(&apply_bytes(&source, &t).unwrap()).unwrap();
            assert_eq!((output.width, output.height), (crop.width, crop.height));
            let input = decompress(&source).unwrap();
            for y in 0..crop.height {
                assert_eq!(&output.data[y * crop.width * 3..(y + 1) * crop.width * 3], &input.data[((crop.y + y) * input.width + crop.x) * 3..((crop.y + y) * input.width + crop.x + crop.width) * 3]);
            }
        }
    }
}

#[test]
fn no_zoom_keeps_size_and_bytes_even_with_pan() {
    let source = jpeg(83, 57, Subsampling::S420);
    let t = PhotoTransform { zoom: 1.0, pan_x: 1.0, pan_y: -1.0, ..Default::default() };
    assert!(crop_region(&source, &t).unwrap().is_none());
    assert_eq!(apply_bytes(&source, &t).unwrap(), source);
}

#[test]
fn partial_edges_are_trimmed_for_the_transform() {
    let source = jpeg(83, 57, Subsampling::S420);
    for (rotation, h, v, expected) in [(0, true, false, (80, 57)), (0, false, true, (83, 48)), (180, false, false, (80, 48)), (90, false, false, (48, 83)), (270, false, false, (57, 80))] {
        let t = PhotoTransform { rotation, flip_h: h, flip_v: v, ..Default::default() };
        let info = probe(&apply_bytes(&source, &t).unwrap()).unwrap();
        assert_eq!((info.width, info.height), expected);
    }
}

#[test]
fn raw_and_dng_files_are_byte_identical() {
    let t = PhotoTransform { flip_h: true, rotation: 90, zoom: 3.0, ..Default::default() };
    let raw = b"II\x2a\0raw sensor data";
    assert_eq!(apply_bytes(raw, &t).unwrap(), raw);
    let folder = std::env::temp_dir().join(format!("telescope-photo-raw-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    for extension in ["dng", "dng.part", "NEF.part"] {
        let file = folder.join(format!("photo.{extension}"));
        let original = jpeg(32, 32, Subsampling::S444);
        std::fs::write(&file, &original).unwrap();
        apply_file(&file, &t).unwrap();
        assert_eq!(std::fs::read(&file).unwrap(), original);
    }
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn unsupported_non_raw_photo_is_rejected_without_changing_bytes() {
    let file = std::env::temp_dir().join(format!("telescope-photo-unsupported-{}.heic.part", std::process::id()));
    let original = b"\0\0\0\x18ftypheic unsupported photo";
    std::fs::write(&file, original).unwrap();
    assert!(apply_file(&file, &PhotoTransform::default()).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), original);
    std::fs::remove_file(file).unwrap();
}

fn exif(orientation: u16, le: bool) -> Vec<u8> {
    let mut tiff = if le { b"II\x2a\0\x08\0\0\0".to_vec() } else { b"MM\0\x2a\0\0\0\x08".to_vec() };
    let short = |n: u16| if le { n.to_le_bytes() } else { n.to_be_bytes() };
    let long = |n: u32| if le { n.to_le_bytes() } else { n.to_be_bytes() };
    tiff.extend(short(1));
    tiff.extend(short(0x112));
    tiff.extend(short(3));
    tiff.extend(long(1));
    tiff.extend(short(orientation));
    tiff.extend(short(0));
    tiff.extend(long(0));
    let mut marker = vec![0xff, 0xe1];
    marker.extend(((tiff.len() + 8) as u16).to_be_bytes());
    marker.extend(b"Exif\0\0");
    marker.extend(tiff);
    marker
}

#[test]
fn exif_is_baked_before_manual_transforms_and_reset() {
    let original = jpeg(64, 48, Subsampling::S444);
    for le in [true, false] {
        let mut source = original.clone();
        source.splice(2..2, exif(6, le));
        let upright = PhotoTransform { rotation: 90, ..Default::default() };
        let t = PhotoTransform { flip_h: true, zoom: 2.0, pan_x: 1.0, ..Default::default() };
        let expected = apply_bytes(&apply_bytes(&original, &upright).unwrap(), &t).unwrap();
        let output = apply_bytes(&source, &t).unwrap();
        assert_eq!(probe(&output).unwrap().exif_orientation, Some(1));
        assert_eq!(decompress(&output).unwrap().data, decompress(&expected).unwrap().data);
    }
}
