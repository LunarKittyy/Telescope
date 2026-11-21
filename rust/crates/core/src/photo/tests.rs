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
    let file = std::env::temp_dir().join(format!("telescope-photo-unsupported-{}.png.part", std::process::id()));
    let original = b"\x89PNG\r\n\x1a\n not a photo that can be turned";
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

use libjpeg_turbo_rs::TransformOp as Op;

const OPS: [Op; 8] = [Op::None, Op::HFlip, Op::VFlip, Op::Rot180, Op::Rot90, Op::Rot270, Op::Transpose, Op::Transverse];

#[test]
fn turns_add_up_like_the_turns_they_are() {
    for rotation in [0, 90, 180, 270] {
        for (flip_h, flip_v) in [(false, false), (true, false), (false, true), (true, true)] {
            let wanted = operation(&PhotoTransform { rotation, flip_h, flip_v, ..Default::default() });
            let flips = match (flip_h, flip_v) { (false, false) => Op::None, (true, false) => Op::HFlip, (false, true) => Op::VFlip, (true, true) => Op::Rot180 };
            let quarter = [Op::None, Op::Rot90, Op::Rot180, Op::Rot270][rotation as usize / 90];
            assert_eq!(orient::then(flips, quarter), wanted, "rotation={rotation} h={flip_h} v={flip_v}");
        }
    }
    for a in OPS {
        assert_eq!(orient::then(a, Op::None), a);
        assert_eq!(Op::from_exif_orientation(orient::exif_value(a) as u8), Some(a));
        let (quarters, mirror) = orient::rotate_then_mirror(a);
        let turn = [Op::None, Op::Rot90, Op::Rot180, Op::Rot270][usize::from(quarters)];
        assert_eq!(orient::then(turn, if mirror { Op::HFlip } else { Op::None }), a);
        for b in OPS {
            // Whatever b does after a is undone by turning the other way
            let total = orient::then(a, b);
            assert!(OPS.iter().any(|undo| orient::then(total, *undo) == a), "{a:?} then {b:?}");
        }
    }
}

/// A photo that already has an EXIF orientation, a thumbnail pointer and a second picture behind it, like an Ultra HDR photo
fn multi_picture(orientation: u16, le: bool) -> (Vec<u8>, Vec<u8>) {
    let base = jpeg(64, 48, Subsampling::S420);
    let mut primary = base.clone();
    let mpf = [b"\xff\xe2\x00\x0aMPF\0".to_vec(), vec![0, 0, 0, 0]].concat();
    primary.splice(2..2, [exif(orientation, le), mpf].concat());
    let gain_map = jpeg(16, 12, Subsampling::S420);
    let mut file = primary.clone();
    file.extend(&gain_map);
    (file, primary)
}

#[test]
fn a_photo_with_a_gain_map_only_changes_its_orientation_tag() {
    for le in [true, false] {
        for start in 1..=8u16 {
            let (file, _) = multi_picture(start, le);
            let now = Op::from_exif_orientation(start as u8).unwrap();
            for t in [PhotoTransform { flip_h: true, ..Default::default() }, PhotoTransform { rotation: 90, flip_v: true, zoom: 3.0, pan_x: 1.0, ..Default::default() },
                      PhotoTransform { rotation: 270, ..Default::default() }, PhotoTransform { rotation: 180, flip_h: true, flip_v: true, ..Default::default() }] {
                let output = apply_bytes(&file, &t).unwrap();
                assert_eq!(output.len(), file.len(), "nothing is added or taken out, so the gain map stays where it is");
                let wanted = orient::exif_value(orient::then(now, operation(&t)));
                assert_eq!(probe(&output).unwrap().exif_orientation, Some(wanted as u8), "start {start} le {le}");
                let differing: Vec<usize> = (0..file.len()).filter(|i| file[*i] != output[*i]).collect();
                assert!(differing.len() <= 6, "only the tag and the thumbnail pointer change: {differing:?}");
                assert!(output.ends_with(&jpeg(16, 12, Subsampling::S420)));
            }
        }
    }
}

#[test]
fn the_tag_route_and_the_pixel_route_show_the_same_picture() {
    let source = exact_pixel_jpeg(64, 48, Subsampling::S444);
    for start in 1..=8u16 {
        let mut tagged = source.clone();
        tagged.splice(2..2, exif(start, true));
        let mut multi = tagged.clone();
        multi.splice(2..2, b"\xff\xe2\x00\x0aMPF\0\0\0\0\0".to_vec());
        for rotation in [0, 90, 180, 270] {
            for (flip_h, flip_v) in [(false, false), (true, false), (false, true), (true, true)] {
                let t = PhotoTransform { rotation, flip_h, flip_v, ..Default::default() };
                let by_pixels = apply_bytes(&tagged, &t).unwrap();
                let by_tag = apply_bytes(&multi, &t).unwrap();
                // Showing a tagged photo means turning it by the tag first
                let shown = apply_bytes(&by_tag, &PhotoTransform::default()).unwrap();
                let upright = transform(&shown, Op::from_exif_orientation(probe(&shown).unwrap().exif_orientation.unwrap_or(1)).unwrap(), None).unwrap();
                assert_eq!(decompress(&upright).unwrap().data, decompress(&by_pixels).unwrap().data, "start {start} rotation={rotation} h={flip_h} v={flip_v}");
            }
        }
    }
}

#[test]
fn a_photo_with_a_gain_map_and_no_orientation_tag_is_refused() {
    let mut file = jpeg(32, 32, Subsampling::S444);
    file.splice(2..2, b"\xff\xe2\x00\x0aMPF\0\0\0\0\0".to_vec());
    let before = file.clone();
    assert!(apply_bytes(&file, &PhotoTransform { flip_h: true, ..Default::default() }).is_err());
    assert_eq!(apply_bytes(&file, &PhotoTransform::default()).unwrap(), before, "nothing to turn, nothing to refuse");
}

fn bx(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    [(body.len() as u32 + 8).to_be_bytes().as_slice(), kind, body].concat()
}

fn full(kind: &[u8; 4], version: u8, body: &[u8]) -> Vec<u8> {
    bx(kind, &[&[version, 0, 0, 0], body].concat())
}

/// A HEIC with a primary picture (item 1), an optional thumbnail (item 2) that shares the turn, and the pictures' bytes in an mdat before or after the meta box
fn heic(irot: Option<u8>, imir: Option<u8>, thumbnail: bool, mdat_first: bool, version: u8) -> Vec<u8> {
    let ftyp = bx(b"ftyp", b"heic\0\0\0\0mif1heic");
    let mut payload = b"PRIMARY-PICTURE-BYTES".to_vec();
    let thumb_at = payload.len();
    if thumbnail { payload.extend(b"THUMB"); }
    let build = |first: u32| {
        let mut props = vec![bx(b"ispe", &[0; 12]), bx(b"hvcC", &[1, 2, 3, 4, 5])];
        let mut shared = vec![1u8, 2];
        for (kind, value) in [(b"irot", irot), (b"imir", imir)] {
            if let Some(v) = value {
                props.push(bx(kind, &[v]));
                shared.push(props.len() as u8);
            }
        }
        let ipco = bx(b"ipco", &props.concat());
        let mut entries = vec![(1u16, shared.clone())];
        if thumbnail { entries.push((2, shared)); }
        let mut ipma_body = (entries.len() as u32).to_be_bytes().to_vec();
        for (item, list) in entries {
            if version == 0 { ipma_body.extend(item.to_be_bytes()); } else { ipma_body.extend(u32::from(item).to_be_bytes()); }
            ipma_body.push(list.len() as u8);
            ipma_body.extend(list.iter().enumerate().map(|(n, p)| if n == 1 { p | 0x80 } else { *p }));
        }
        let iprp = bx(b"iprp", &[ipco, full(b"ipma", version, &ipma_body)].concat());
        let mut iloc = vec![0x44, 0x00];
        iloc.extend((1 + u16::from(thumbnail)).to_be_bytes());
        iloc.extend([0, 1, 0, 0, 0, 1]);
        iloc.extend((first + 8).to_be_bytes());
        iloc.extend((thumb_at as u32).to_be_bytes());
        if thumbnail {
            iloc.extend([0, 2, 0, 0, 0, 1]);
            iloc.extend((first + 8 + thumb_at as u32).to_be_bytes());
            iloc.extend(5u32.to_be_bytes());
        }
        let mut kids = vec![full(b"hdlr", 0, &[0; 20]), full(b"pitm", 0, &[0, 1]), full(b"iloc", 0, &iloc)];
        if thumbnail {
            kids.push(full(b"iref", 0, &bx(b"thmb", &[0, 2, 0, 1, 0, 1])));
        }
        kids.push(iprp);
        full(b"meta", 0, &kids.concat())
    };
    let mdat = bx(b"mdat", &payload);
    let meta_length = build(0).len() as u32;
    if mdat_first {
        let first = ftyp.len() as u32;
        [ftyp, mdat, build(first)].concat()
    } else {
        let first = ftyp.len() as u32 + meta_length;
        [ftyp, build(first), mdat].concat()
    }
}

/// The top-level boxes of a file, and a check that they add up to all of it
fn walk(data: &[u8]) -> Vec<([u8; 4], usize, usize)> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < data.len() {
        let size = u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
        found.push((data[at + 4..at + 8].try_into().unwrap(), at, at + size));
        at += size;
    }
    assert_eq!(at, data.len(), "the boxes add up to the whole file");
    found
}

/// What the item's extent in iloc points at
fn item(data: &[u8], id: u16) -> Vec<u8> {
    let at = data.windows(4).position(|w| w == b"iloc").unwrap() + 4;
    let count = u16::from_be_bytes([data[at + 6], data[at + 7]]) as usize;
    let mut at = at + 8;
    for _ in 0..count {
        let this = u16::from_be_bytes([data[at], data[at + 1]]);
        let offset = u32::from_be_bytes(data[at + 6..at + 10].try_into().unwrap()) as usize;
        let length = u32::from_be_bytes(data[at + 10..at + 14].try_into().unwrap()) as usize;
        if this == id { return data[offset..offset + length].to_vec(); }
        at += 14;
    }
    panic!("no item {id}");
}

fn turns(data: &[u8]) -> Vec<(u64, Op)> {
    heif::turns(data).unwrap()
}

#[test]
fn a_heic_is_turned_by_its_properties_and_everything_still_lines_up() {
    let angle = |a: u8| [Op::None, Op::Rot270, Op::Rot180, Op::Rot90][usize::from(a)];
    for (irot, imir) in [(None, None), (Some(0), None), (Some(1), None), (Some(3), Some(0)), (None, Some(1)), (Some(2), Some(0)), (Some(1), Some(1))] {
        for thumbnail in [false, true] {
            for mdat_first in [false, true] {
                for version in [0, 1] {
                    let file = heic(irot, imir, thumbnail, mdat_first, version);
                    assert!(heif::is_heif(&file));
                    let before = turns(&file);
                    let start = orient::then(irot.map_or(Op::None, angle), imir.map_or(Op::None, |a| if a == 0 { Op::VFlip } else { Op::HFlip }));
                    assert_eq!(before.len(), 1 + usize::from(thumbnail));
                    assert!(before.iter().all(|(_, turn)| *turn == start));
                    for op in OPS {
                        let out = heif::turn(&file, op).unwrap();
                        let after = turns(&out);
                        assert_eq!(after.len(), before.len());
                        for (_, turn) in &after {
                            assert_eq!(*turn, orient::then(start, op), "irot={irot:?} imir={imir:?} then {op:?}");
                        }
                        walk(&out);
                        assert_eq!(item(&out, 1), b"PRIMARY-PICTURE-BYTES", "the picture moves with the header (mdat first: {mdat_first})");
                        if thumbnail { assert_eq!(item(&out, 2), b"THUMB"); }
                        // Turning back leaves the picture the way it was
                        let twice = heif::turn(&out, OPS.into_iter().find(|undo| orient::then(op, *undo) == Op::None).unwrap()).unwrap();
                        assert!(turns(&twice).iter().all(|(_, turn)| *turn == start));
                    }
                    assert_eq!(heif::turn(&file, Op::None).unwrap(), file);
                }
            }
        }
    }
}

#[test]
fn a_heic_file_is_changed_on_disk_and_a_damaged_one_is_left_alone() {
    let folder = std::env::temp_dir().join(format!("telescope-photo-heic-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let file = folder.join("IMG_1.heic.part");
    let original = heic(Some(1), None, true, false, 0);
    std::fs::write(&file, &original).unwrap();
    // Zoom is ignored, the turn is not
    apply_file(&file, &PhotoTransform { flip_h: true, zoom: 4.0, pan_x: 1.0, ..Default::default() }).unwrap();
    let turned = std::fs::read(&file).unwrap();
    assert!(turned.len() > original.len());
    assert!(turns(&turned).iter().all(|(_, t)| *t == orient::then(Op::Rot270, Op::HFlip)));
    apply_file(&file, &PhotoTransform::default()).unwrap();
    assert_eq!(std::fs::read(&file).unwrap(), turned, "nothing to change");
    std::fs::write(&file, &original[..original.len() - 40]).unwrap();
    assert!(apply_file(&file, &PhotoTransform { rotation: 90, ..Default::default() }).is_err());
    assert_eq!(std::fs::read(&file).unwrap(), &original[..original.len() - 40]);
    std::fs::remove_dir_all(folder).unwrap();
}

#[test]
fn a_heic_that_cannot_be_turned_is_refused_and_never_panics() {
    let file = heic(Some(1), Some(0), true, false, 0);
    for end in 0..file.len() {
        let _ = heif::turn(&file[..end], Op::Rot90);
        let _ = heif::is_heif(&file[..end]);
    }
    let mut seed = 0x2545f491u32;
    for _ in 0..4000 {
        let mut damaged = file.clone();
        for _ in 0..1 + seed % 3 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let at = (seed >> 8) as usize % damaged.len();
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            damaged[at] = (seed >> 16) as u8;
        }
        let _ = heif::turn(&damaged, Op::Transverse);
        let _ = heif::is_heif(&damaged);
    }
    let mut sequence = file.clone();
    sequence.extend(bx(b"moov", &[]));
    assert!(heif::turn(&sequence, Op::Rot90).is_err());
}

#[test]
fn damaged_jpegs_are_refused_or_turned_and_never_panic() {
    let sources = [include_bytes!("../../../media/fixtures/jpeg/cv2_64x48.jpg").to_vec(), include_bytes!("../../../media/fixtures/jpeg/cv2_33x7.jpg").to_vec(),
                   include_bytes!("../../../media/fixtures/jpeg/cv2_80x60_progressive.jpg").to_vec(), multi_picture(6, true).0];
    let t = PhotoTransform { flip_h: true, rotation: 90, zoom: 2.0, pan_x: 0.5, ..Default::default() };
    let mut seed = 0x9e3779b9u32;
    for source in &sources {
        for end in (0..source.len()).step_by(7) {
            let _ = std::panic::catch_unwind(|| apply_unchecked(&source[..end], &t)).unwrap_or_else(|_| panic!("cut at {end}"));
        }
        for round in 0..300 {
            let mut damaged = source.clone();
            for _ in 0..1 + seed % 4 {
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                let at = (seed >> 8) as usize % damaged.len();
                seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                damaged[at] = (seed >> 16) as u8;
            }
            let _ = std::panic::catch_unwind(|| apply_unchecked(&damaged, &t)).unwrap_or_else(|_| panic!("round {round}"));
        }
    }
}

/// The photo cut down to whole MCUs on the edges a turn needs them on, which is what turning it is expected to keep
fn trimmed_source(source: &[u8], t: &PhotoTransform) -> Vec<u8> {
    let f = frame(source).unwrap();
    let (trim_width, trim_height) = edges_to_trim(operation(t));
    let width = if trim_width { f.width / f.mcu_width * f.mcu_width } else { f.width };
    let height = if trim_height { f.height / f.mcu_height * f.mcu_height } else { f.height };
    if (width, height) == (f.width, f.height) { source.to_vec() } else { crop_only(source, CropRegion { x: 0, y: 0, width, height }).unwrap() }
}

#[test]
fn a_turned_photo_is_a_whole_picture_at_every_size_and_sampling() {
    for sampling in [Subsampling::S444, Subsampling::S420, Subsampling::S422] {
        for (width, height) in [(24, 24), (83, 57), (33, 49), (17, 17), (40, 24), (24, 40), (64, 48)] {
            let source = exact_pixel_jpeg(width, height, sampling);
            for rotation in [0, 90, 180, 270] {
                for flip_h in [false, true] {
                    for flip_v in [false, true] {
                        let t = PhotoTransform { rotation, flip_h, flip_v, ..Default::default() };
                        let name = format!("{width}x{height} {sampling:?} rotation={rotation} h={flip_h} v={flip_v}");
                        let output = apply_bytes(&source, &t).unwrap_or_else(|e| panic!("{name}: {e}"));
                        verify_jpeg(&output).unwrap_or_else(|e| panic!("{name}: {e}"));
                        let info = probe(&output).unwrap();
                        let image = decompress(&output).unwrap_or_else(|e| panic!("{name}: {e}"));
                        assert_eq!((image.width, image.height), (info.width, info.height), "{name}");
                        let expected = reference(&trimmed_source(&source, &t), &t);
                        assert_eq!((image.width, image.height), (expected.0, expected.1), "{name}");
                        assert!(image.data == expected.2, "{name}: the pixels are not the turned pixels");
                    }
                }
            }
        }
    }
}

#[test]
fn a_24_by_24_photo_turned_a_quarter_keeps_both_mcu_rows() {
    let source = exact_pixel_jpeg(24, 24, Subsampling::S420);
    for rotation in [90, 270] {
        let output = apply_bytes(&source, &PhotoTransform { rotation, ..Default::default() }).unwrap();
        let image = decompress(&output).unwrap();
        let (width, height) = if rotation == 90 { (16, 24) } else { (24, 16) };
        assert_eq!((image.width, image.height), (width, height));
        let coefficients = libjpeg_turbo_rs::read_coefficients(&output).unwrap();
        let luma = &coefficients.components[0];
        assert_eq!((luma.blocks_x, luma.blocks_y), (width.div_ceil(16) * 2, height.div_ceil(16) * 2));
    }
}

#[test]
fn a_scan_that_is_too_short_is_not_accepted() {
    let source = exact_pixel_jpeg(48, 48, Subsampling::S420);
    verify_jpeg(&source).unwrap();
    // The scan is cut short and closed again, as a writer that drops a row would leave it
    let scan = source.windows(2).rposition(|w| w == [0xff, 0xda]).unwrap();
    let mut short = source[..scan + 24].to_vec();
    short.extend([0xff, 0xd9]);
    assert!(verify_jpeg(&short).is_err());
    assert!(verify_jpeg(&source[..source.len() / 2]).is_err());
}

fn with_sampling(mut data: Vec<u8>, factors: &[u8]) -> Vec<u8> {
    let (_, start, _) = app_segments(&data).into_iter().find(|s| s.0 == 0xc0).unwrap();
    for (n, factor) in factors.iter().enumerate() { data[start + 7 + n * 3] = *factor; }
    data
}

#[test]
fn the_crop_follows_the_largest_sampling_factors_of_the_frame() {
    let source = with_sampling(exact_pixel_jpeg(96, 48, Subsampling::S444), &[0x31, 0x21, 0x21]);
    let f = frame(&source).unwrap();
    assert_eq!((f.mcu_width, f.mcu_height), (24, 8));
    for pan in [-1.0, 0.0, 0.4, 1.0] {
        let crop = crop_region(&source, &PhotoTransform { zoom: 3.0, pan_x: pan, ..Default::default() }).unwrap().unwrap();
        assert_eq!((crop.x % 24, crop.width % 24), (0, 0), "pan {pan}");
        assert!(crop.width > 0 && crop.x + crop.width <= 96);
    }
    let centred = crop_region(&source, &PhotoTransform { zoom: 3.0, ..Default::default() }).unwrap().unwrap();
    assert_eq!((centred.x, centred.width), (24, 24));
    let plain = frame(&exact_pixel_jpeg(96, 48, Subsampling::S420)).unwrap();
    assert_eq!((plain.mcu_width, plain.mcu_height), (16, 16));
    let grey = frame(&compress(&[0u8; 20 * 20], 20, 20, PixelFormat::Grayscale, 90, Subsampling::S444).unwrap()).unwrap();
    assert_eq!((grey.width, grey.height, grey.components), (20, 20, 1));
}

#[test]
fn a_photo_that_cannot_be_edited_is_kept_exactly_as_the_camera_took_it() {
    let dir = crate::testing::scratch_dir("kept");
    // An Ultra HDR photo with a gain map and no orientation tag can't be turned
    let mut ultra = jpeg(32, 32, Subsampling::S444);
    ultra.splice(2..2, b"\xff\xe2\x00\x0aMPF\0\0\0\0\0".to_vec());
    ultra.extend(jpeg(8, 8, Subsampling::S444));
    let cases = [("ultra.jpg.part", ultra), ("damaged.jpg.part", jpeg(32, 32, Subsampling::S444)[..300].to_vec()), ("odd.heic.part", heic(Some(1), None, false, false, 0)[..120].to_vec())];
    for (name, original) in cases {
        let file = dir.join(name);
        std::fs::write(&file, &original).unwrap();
        let t = PhotoTransform { rotation: 90, flip_h: true, zoom: 2.0, ..Default::default() };
        assert!(apply_file(&file, &t).is_err(), "{name}");
        assert_eq!(std::fs::read(&file).unwrap(), original, "{name}");
        assert!(apply_file_or_keep(&file, &t).is_some(), "{name}");
        assert_eq!(std::fs::read(&file).unwrap(), original, "{name}");
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1, "{name}: no temporary file is left");
        std::fs::remove_file(&file).unwrap();
    }
    // A successful edit replaces the file and leaves nothing beside it
    let file = dir.join("fine.jpg.part");
    let original = jpeg(64, 48, Subsampling::S420);
    std::fs::write(&file, &original).unwrap();
    assert!(apply_file_or_keep(&file, &PhotoTransform { rotation: 90, ..Default::default() }).is_none());
    assert_ne!(std::fs::read(&file).unwrap(), original);
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
}

#[test]
fn a_photo_too_small_to_turn_is_turned_by_its_tag_or_left_alone() {
    let mut tagged = jpeg(8, 8, Subsampling::S420);
    tagged.splice(2..2, exif(1, true));
    let output = apply_bytes(&tagged, &PhotoTransform { rotation: 90, ..Default::default() }).unwrap();
    assert_eq!(probe(&output).unwrap().exif_orientation, Some(6));
    assert_eq!(decompress(&output).unwrap().data, decompress(&tagged).unwrap().data);
    let plain = jpeg(8, 8, Subsampling::S420);
    assert!(apply_bytes(&plain, &PhotoTransform { rotation: 90, ..Default::default() }).is_err());
    assert_eq!(apply_bytes(&plain, &PhotoTransform::default()).unwrap(), plain);
}

/// What a test HEIC is made of. Item 1 is the picture, and the picture's bytes are in an mdat after the meta box.
struct Parts {
    props: Vec<Vec<u8>>,
    /// One list per ipma box, of an item and the property numbers it has
    tables: Vec<Vec<(u16, Vec<u8>)>>,
    /// Boxes inside iref
    refs: Vec<Vec<u8>>,
    /// An idat box after iprp, with item 2 pointing into it by file offset and item 3 by offset into the idat
    idat: bool,
    extended_ftyp: bool,
    open_mdat: bool,
}

const PIXELS: &[u8] = b"PRIMARY-PICTURE-BYTES";
const INSIDE: &[u8] = b"INSIDE-THE-IDAT";

fn parts() -> Parts {
    Parts { props: vec![bx(b"ispe", &[0; 12]), bx(b"hvcC", &[1, 2, 3, 4, 5])], tables: vec![vec![(1, vec![1, 2])]], refs: Vec::new(), idat: false, extended_ftyp: false, open_mdat: false }
}

fn built(parts: &Parts) -> Vec<u8> {
    let ftyp = if parts.extended_ftyp {
        let brands = b"heic\0\0\0\0mif1heic";
        [&[0u8, 0, 0, 1][..], b"ftyp", &(brands.len() as u64 + 16).to_be_bytes(), brands].concat()
    } else {
        bx(b"ftyp", b"heic\0\0\0\0mif1heic")
    };
    let make = |mdat_at: u32, idat_at: u32| {
        let mut iprp = vec![bx(b"ipco", &parts.props.concat())];
        for table in &parts.tables {
            let mut body = (table.len() as u32).to_be_bytes().to_vec();
            for (item, list) in table {
                body.extend(item.to_be_bytes());
                body.push(list.len() as u8);
                body.extend(list);
            }
            iprp.push(full(b"ipma", 0, &body));
        }
        let mut entries: Vec<(u16, u16, u32, u32)> = vec![(1, 0, mdat_at, PIXELS.len() as u32)];
        if parts.idat {
            entries.push((2, 0, idat_at + 7, 3));
            entries.push((3, 1, 9, 6));
        }
        let mut iloc = vec![0x44, 0x00];
        iloc.extend((entries.len() as u16).to_be_bytes());
        for (item, method, offset, length) in entries {
            iloc.extend(item.to_be_bytes());
            iloc.extend(method.to_be_bytes());
            iloc.extend([0, 0, 0, 1]);
            iloc.extend(offset.to_be_bytes());
            iloc.extend(length.to_be_bytes());
        }
        let mut kids = vec![full(b"hdlr", 0, &[0; 20]), full(b"pitm", 0, &[0, 1]), full(b"iloc", 1, &iloc)];
        if !parts.refs.is_empty() { kids.push(full(b"iref", 0, &parts.refs.concat())); }
        kids.push(bx(b"iprp", &iprp.concat()));
        if parts.idat { kids.push(bx(b"idat", INSIDE)); }
        full(b"meta", 0, &kids.concat())
    };
    let first = make(0, 0);
    let idat_at = if parts.idat { (ftyp.len() + first.windows(4).position(|w| w == b"idat").unwrap() + 4) as u32 } else { 0 };
    let mdat_at = (ftyp.len() + first.len() + 8) as u32;
    let mut mdat = bx(b"mdat", PIXELS);
    if parts.open_mdat { mdat[..4].fill(0); }
    [ftyp, make(mdat_at, idat_at), mdat].concat()
}

/// The items in iloc as (item, construction method, offset, length)
fn iloc_entries(data: &[u8]) -> Vec<(u16, u16, usize, usize)> {
    let at = data.windows(4).position(|w| w == b"iloc").unwrap() + 4;
    assert_eq!(data[at], 1);
    let be16 = |at: usize| usize::from(u16::from_be_bytes([data[at], data[at + 1]]));
    let be32 = |at: usize| u32::from_be_bytes(data[at..at + 4].try_into().unwrap()) as usize;
    (0..be16(at + 6)).map(|n| { let e = at + 8 + n * 16; (be16(e) as u16, (be16(e + 2) & 15) as u16, be32(e + 8), be32(e + 12)) }).collect()
}

fn idat_payload(data: &[u8]) -> usize {
    data.windows(4).position(|w| w == b"idat").unwrap() + 4
}

#[test]
fn item_data_inside_the_meta_box_follows_the_header_that_grows_in_front_of_it() {
    let mut p = parts();
    p.idat = true;
    p.props.push(bx(b"irot", &[1]));
    p.tables = vec![vec![(1, vec![1, 2, 0x83])]];
    let file = built(&p);
    assert!(idat_payload(&file) > file.windows(4).position(|w| w == b"ipma").unwrap(), "the idat is after the associations");
    for op in OPS.into_iter().filter(|op| *op != Op::None) {
        let out = heif::turn(&file, op).unwrap();
        assert_ne!(out.len(), file.len());
        let entries = iloc_entries(&out);
        let (_, _, offset, length) = entries[0];
        assert_eq!(&out[offset..offset + length], PIXELS, "{op:?}");
        let (_, method, offset, length) = entries[1];
        assert_eq!(method, 0);
        assert_eq!(&out[offset..offset + length], b"THE", "an absolute extent into the idat moves with it, {op:?}");
        let (_, method, offset, length) = entries[2];
        assert_eq!((method, offset), (1, 9), "an extent counted from the idat stays as it is");
        assert_eq!(&out[idat_payload(&out) + offset..][..length], b"E-IDAT");
        walk(&out);
    }
}

#[test]
fn a_heic_that_cannot_be_turned_exactly_is_declined_and_left_alone() {
    let mut shapes = Vec::new();
    // A crop or a scaling by the properties, whichever way round they come
    for (kind, body) in [(b"clap", vec![0; 32]), (b"iscl", vec![0; 4])] {
        for turned_first in [false, true] {
            let mut p = parts();
            p.props.push(bx(b"irot", &[1]));
            p.props.push(bx(kind, &body));
            p.tables = vec![vec![(1, if turned_first { vec![1, 2, 0x83, 0x84] } else { vec![1, 2, 0x84, 0x83] })]];
            shapes.push(p);
        }
    }
    // An alpha mask or a depth map for the picture
    let mut alpha = parts();
    alpha.refs = vec![bx(b"auxl", &[0, 5, 0, 1, 0, 1])];
    shapes.push(alpha);
    // The picture used as a tile of a grid
    let mut tile = parts();
    tile.refs = vec![bx(b"dimg", &[0, 6, 0, 2, 0, 3, 0, 1])];
    shapes.push(tile);
    // Alpha for a thumbnail that turns with the picture
    let mut thumbnail = parts();
    thumbnail.tables = vec![vec![(1, vec![1, 2]), (2, vec![1, 2])]];
    thumbnail.refs = vec![bx(b"thmb", &[0, 2, 0, 1, 0, 1]), bx(b"auxl", &[0, 7, 0, 1, 0, 2])];
    shapes.push(thumbnail);
    let folder = crate::testing::scratch_dir("declined-heic");
    for (n, p) in shapes.iter().enumerate() {
        let file = built(p);
        for op in OPS.into_iter().filter(|op| *op != Op::None) {
            assert!(heif::turn(&file, op).is_err(), "shape {n} {op:?}");
        }
        let path = folder.join(format!("{n}.heic.part"));
        std::fs::write(&path, &file).unwrap();
        assert!(apply_file_or_keep(&path, &PhotoTransform { rotation: 90, ..Default::default() }).is_some());
        assert_eq!(std::fs::read(&path).unwrap(), file, "shape {n} is saved as it came");
    }
    // The same pictures with the references pointing at other items are turned
    let mut fine = parts();
    fine.refs = vec![bx(b"auxl", &[0, 5, 0, 1, 0, 8]), bx(b"dimg", &[0, 6, 0, 2, 0, 3, 0, 4])];
    assert!(heif::turn(&built(&fine), Op::Rot90).is_ok());
}

#[test]
fn heic_files_with_several_ipma_boxes_long_sizes_and_open_ended_boxes_are_turned() {
    let mut p = parts();
    p.props.push(bx(b"irot", &[1]));
    p.tables = vec![vec![(1, vec![1, 2, 0x83])], vec![(2, vec![1, 2, 0x83])]];
    p.refs = vec![bx(b"thmb", &[0, 2, 0, 1, 0, 1])];
    for extended_ftyp in [false, true] {
        for open_mdat in [false, true] {
            p.extended_ftyp = extended_ftyp;
            p.open_mdat = open_mdat;
            let file = built(&p);
            assert!(heif::is_heif(&file), "extended {extended_ftyp}");
            let before = turns(&file);
            assert_eq!(before.len(), 2);
            assert!(before.iter().all(|(_, turn)| *turn == Op::Rot270));
            for op in [Op::HFlip, Op::Rot90, Op::Transverse] {
                let out = heif::turn(&file, op).unwrap();
                let after = turns(&out);
                assert_eq!(after.len(), 2);
                assert!(after.iter().all(|(_, turn)| *turn == orient::then(Op::Rot270, op)), "extended {extended_ftyp} open {open_mdat} {op:?}");
                let (_, _, offset, length) = iloc_entries(&out)[0];
                assert_eq!(&out[offset..offset + length], PIXELS);
            }
        }
    }
}

#[test]
fn a_heic_with_boxes_too_short_for_what_they_hold_is_refused() {
    let file = built(&parts());
    for kind in [b"pitm", b"iloc", b"ipma", b"iprp"] {
        let at = file.windows(4).position(|w| w == kind).unwrap();
        let mut cut = file.clone();
        cut[at - 4..at].copy_from_slice(&8u32.to_be_bytes());
        assert!(heif::turn(&cut, Op::Rot90).is_err(), "{}", String::from_utf8_lossy(kind));
        let _ = heif::is_heif(&cut);
    }
}
