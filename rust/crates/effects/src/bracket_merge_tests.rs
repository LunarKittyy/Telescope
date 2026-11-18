use super::*;

const W: usize = 640;
const H: usize = 480;

/// A random value in -1..1 at a corner of a grid.
fn corner(ix: i64, iy: i64, seed: u32) -> f64 {
    let mut h = (ix as u32).wrapping_mul(0x9E37_79B1) ^ (iy as u32).wrapping_mul(0x85EB_CA6B) ^ seed.wrapping_mul(0xC2B2_AE35);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    h as f64 / u32::MAX as f64 * 2.0 - 1.0
}

/// Smooth random texture with features about `cell` pixels across.
fn noise(x: f64, y: f64, cell: f64, seed: u32) -> f64 {
    let (gx, gy) = (x / cell, y / cell);
    let (ix, iy) = (gx.floor() as i64, gy.floor() as i64);
    let (fx, fy) = (gx - ix as f64, gy - iy as f64);
    let ease = |t: f64| t * t * (3.0 - 2.0 * t);
    let (u, v) = (ease(fx), ease(fy));
    let top = corner(ix, iy, seed) * (1.0 - u) + corner(ix + 1, iy, seed) * u;
    let bottom = corner(ix, iy + 1, seed) * (1.0 - u) + corner(ix + 1, iy + 1, seed) * u;
    top * (1.0 - v) + bottom * v
}

/// The scene at a point: the blurred picture's coarse texture, and the fine detail only a sharp photo has.
fn scene(x: f64, y: f64, sharp: bool) -> [f64; 3] {
    let base = [0u32, 1, 2].map(|c| 0.5 + 0.25 * noise(x, y, 60.0, c) + 0.15 * noise(x, y, 24.0, c + 7));
    if !sharp {
        return base;
    }
    let fine = noise(x, y, 4.0, 40) * 0.7 + noise(x, y, 2.5, 41) * 0.5;
    base.map(|b| b + 0.2 * fine)
}

fn render(from: impl Fn(f64, f64) -> [f64; 3]) -> Image {
    let mut rgb = Vec::with_capacity(W * H * 3);
    for y in 0..H {
        for x in 0..W {
            rgb.extend(from(x as f64, y as f64).map(|v| (v * 255.0).clamp(0.0, 255.0) as u8));
        }
    }
    Image { width: W, height: H, rgb }
}

const CENTRE: (f64, f64) = (W as f64 / 2.0, H as f64 / 2.0);

/// Photo i of three: sharp in the third of the picture (by rows) that belongs to it, blurred elsewhere, grown by `grow` about the middle and shifted.
fn photo(i: usize, grow: f64, shift: (f64, f64)) -> Image {
    render(|x, y| {
        // which point of the scene the photo's pixel shows
        let (sx, sy) = (CENTRE.0 + (x - CENTRE.0 - shift.0) / (1.0 + grow), CENTRE.1 + (y - CENTRE.1 - shift.1) / (1.0 + grow));
        let band = ((sy / (H as f64 / 3.0)).floor().clamp(0.0, 2.0)) as usize;
        scene(sx, sy, band == i)
    })
}

/// The truth: the whole scene sharp, in the geometry of the middle photo.
fn truth() -> Image {
    render(|x, y| scene(x, y, true))
}

/// Mean absolute difference of a band of rows (a share of the height) between two pictures, 0..255.
fn error(a: &Image, b: &Image, rows: (f64, f64)) -> f64 {
    let (y0, y1) = ((rows.0 * H as f64) as usize, (rows.1 * H as f64) as usize);
    let (mut sum, mut n) = (0.0, 0usize);
    for y in y0..y1 {
        for x in 40..W - 40 {
            for c in 0..3 {
                sum += (a.rgb[(y * W + x) * 3 + c] as f64 - b.rgb[(y * W + x) * 3 + c] as f64).abs();
                n += 1;
            }
        }
    }
    sum / n as f64
}

fn merged(photos: Vec<Image>) -> Image {
    merge(photos.len(), &|i| photos.get(i).map(|p| Image { width: p.width, height: p.height, rgb: p.rgb.clone() }), Kind::Focus).expect("a merged picture")
}

#[test]
fn every_part_of_the_result_comes_from_the_photo_that_is_sharp_there() {
    // the middle photo is sharp in the middle third only
    let photos = vec![photo(0, 0.0, (0.0, 0.0)), photo(1, 0.0, (0.0, 0.0)), photo(2, 0.0, (0.0, 0.0))];
    let truth = truth();
    let middle = Image { width: W, height: H, rgb: photos[1].rgb.clone() };
    let result = merged(photos);
    // each third, away from where the thirds meet, is nearly as sharp as the truth, where the middle photo alone is far from it
    for rows in [(0.08, 0.25), (0.42, 0.58), (0.75, 0.92)] {
        let (got, alone) = (error(&result, &truth, rows), error(&middle, &truth, rows));
        assert!(got < 6.0, "rows {rows:?}: {got} from the truth");
        if rows != (0.42, 0.58) {
            assert!(alone > 3.0 * got, "rows {rows:?}: the middle photo alone is {alone} off, the merge {got}");
        }
    }
}

#[test]
fn photos_that_grew_and_moved_as_the_focus_changed_are_lined_up_first() {
    // focus breathing: the nearer photo is a little smaller, the farther a little bigger, and the phone moved a pixel or two
    let photos = vec![photo(0, -0.02, (2.0, -1.0)), photo(1, 0.0, (0.0, 0.0)), photo(2, 0.025, (-1.5, 2.0))];
    let truth = truth();
    let result = merged(photos);
    for rows in [(0.08, 0.25), (0.42, 0.58), (0.75, 0.92)] {
        let got = error(&result, &truth, rows);
        assert!(got < 9.0, "rows {rows:?}: {got} from the truth");
    }
}

#[test]
fn the_size_and_shift_of_a_photo_are_found() {
    let reference = luma(&photo(1, 0.0, (0.0, 0.0)), 1).unwrap();
    let moved = luma(&photo(1, 0.03, (3.0, -2.0)), 1).unwrap();
    let warp = estimate(&reference, &moved).expect("a match");
    assert!((warp.scale - 1.03).abs() < 0.004, "scale {}", warp.scale);
    assert!((warp.shift.0 - 3.0).abs() < 1.0 && (warp.shift.1 + 2.0).abs() < 1.0, "shift {:?}", warp.shift);
    // two pictures of nothing in common have no match
    let blank = Plane { width: W, height: H, data: vec![0.5; W * H] };
    assert!(estimate(&reference, &blank).is_none());
}

#[test]
fn a_photo_that_cant_be_lined_up_is_left_out_and_too_few_is_an_error() {
    let photos = [photo(0, 0.0, (0.0, 0.0)), photo(1, 0.0, (0.0, 0.0)), photo(2, 0.0, (0.0, 0.0))];
    let blank = || Image { width: W, height: H, rgb: vec![128; W * H * 3] };
    let result = merge(3, &|i| if i == 0 { Some(blank()) } else { Some(Image { width: W, height: H, rgb: photos[i].rgb.clone() }) }, Kind::Focus);
    assert!(result.is_ok(), "the other two still merge");
    assert!(merge(1, &|_| Some(blank()), Kind::Focus).is_err());
    assert!(merge(2, &|i| (i == 1).then(blank), Kind::Focus).is_err());
}

#[test]
fn photos_are_read_merged_and_saved_next_to_the_first_with_what_the_camera_wrote() {
    let folder = std::env::temp_dir().join(format!("focus-merge-{}", std::process::id()));
    std::fs::create_dir_all(&folder).unwrap();
    let exif = {
        let mut seg = vec![0xFF, 0xE1, 0x00, 0x10];
        seg.extend_from_slice(b"Exif\0\0");
        seg.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
        seg
    };
    let mut files = Vec::new();
    for i in 0..3 {
        let path = folder.join(format!("IMG_{i}.jpg"));
        std::fs::write(&path, encode_jpeg(&photo(i, 0.0, (0.0, 0.0)), 92, Some(&exif)).unwrap()).unwrap();
        files.push(path.display().to_string());
    }
    // both: the photos stay
    let result = merge_files(&files, 90, true, Kind::Focus);
    assert_eq!(result["ok"], json!(true), "{result}");
    let path = PathBuf::from(result["path"].as_str().unwrap());
    assert_eq!(path.file_name().unwrap().to_string_lossy(), "IMG_0_focus-merged.jpg");
    let bytes = std::fs::read(&path).unwrap();
    assert!(exif_of(&bytes).is_some(), "the Exif block comes along");
    assert!(files.iter().all(|f| Path::new(f).exists()));
    // only the merged picture: the photos go, and a second merge doesn't take the first one's name
    let again = merge_files(&files, 90, false, Kind::Focus);
    assert_eq!(again["ok"], json!(true));
    assert_eq!(PathBuf::from(again["path"].as_str().unwrap()).file_name().unwrap().to_string_lossy(), "IMG_0_focus-merged (2).jpg");
    assert!(files.iter().all(|f| !Path::new(f).exists()) && again["removed"] == json!(3));
    // a DNG or a missing file isn't a photo to merge
    assert_eq!(merge_files(&["a.dng".to_string(), "b.dng".to_string()], 90, true, Kind::Focus)["ok"], json!(false));
    std::fs::remove_dir_all(&folder).unwrap();
}

/// The light of a scene at a point (1 is what a photo at the middle exposure turns white): a dark left half with texture and a bright right half with texture.
fn radiance(x: f64, y: f64) -> [f64; 3] {
    let texture = noise(x, y, 14.0, 5) * 0.5 + noise(x, y, 5.0, 6) * 0.35 + 0.5;
    let level = if x < W as f64 / 2.0 { 0.04 + 0.12 * texture } else { 1.2 + 1.6 * texture };
    [level * (0.9 + 0.2 * noise(x, y, 80.0, 1)), level, level * (0.9 + 0.2 * noise(x, y, 80.0, 2))]
}

/// A photo of that scene `stops` brighter than the middle one: clipped at white, and in the display's brightness curve.
fn exposure(stops: f64, object: Option<(usize, usize)>) -> Image {
    render(|x, y| {
        if let Some((ox, oy)) = object {
            if (x as usize).abs_diff(ox) < 40 && (y as usize).abs_diff(oy) < 40 {
                return [0.05, 0.9, 0.05].map(|v: f64| (v * 2f64.powf(stops)).clamp(0.0, 1.0).powf(1.0 / 2.2));
            }
        }
        radiance(x, y).map(|v| (v * 2f64.powf(stops)).clamp(0.0, 1.0).powf(1.0 / 2.2))
    })
}

fn bracket() -> Vec<Image> {
    vec![exposure(-2.0, None), exposure(0.0, None), exposure(2.0, None)]
}

fn fused(photos: &[Image], brightness: i32) -> Image {
    merge(photos.len(), &|i| photos.get(i).map(|p| Image { width: p.width, height: p.height, rgb: p.rgb.clone() }), Kind::Exposure { brightness }).expect("a merged picture")
}

/// How much fine texture a part of a picture has (the mean size of its Laplacian, 0..255) and how bright it is (0..255), over columns and rows given as shares.
fn look(image: &Image, cols: (f64, f64), rows: (f64, f64)) -> (f64, f64) {
    let (x0, x1, y0, y1) = ((cols.0 * W as f64) as usize, (cols.1 * W as f64) as usize, (rows.0 * H as f64) as usize, (rows.1 * H as f64) as usize);
    let g = |x: usize, y: usize| (image.rgb[(y * W + x) * 3] as f64 + image.rgb[(y * W + x) * 3 + 1] as f64 + image.rgb[(y * W + x) * 3 + 2] as f64) / 3.0;
    let (mut detail, mut sum, mut n) = (0.0, 0.0, 0usize);
    for y in y0.max(1)..y1.min(H - 1) {
        for x in x0.max(1)..x1.min(W - 1) {
            detail += (g(x - 1, y) + g(x + 1, y) + g(x, y - 1) + g(x, y + 1) - 4.0 * g(x, y)).abs();
            sum += g(x, y);
            n += 1;
        }
    }
    (detail / n as f64, sum / n as f64)
}

/// How much greener than red and blue a part of a picture is (0..255).
fn green(image: &Image, cols: (f64, f64), rows: (f64, f64)) -> f64 {
    let (x0, x1, y0, y1) = ((cols.0 * W as f64) as usize, (cols.1 * W as f64) as usize, (rows.0 * H as f64) as usize, (rows.1 * H as f64) as usize);
    let (mut sum, mut n) = (0.0, 0usize);
    for y in y0..y1 {
        for x in x0..x1 {
            let p = &image.rgb[(y * W + x) * 3..(y * W + x) * 3 + 3];
            sum += p[1] as f64 - (p[0] as f64 + p[2] as f64) / 2.0;
            n += 1;
        }
    }
    sum / n as f64
}

#[test]
fn an_exposure_bracket_has_detail_in_both_the_dark_and_the_bright_part() {
    let photos = bracket();
    let (dark, bright) = ((0.05, 0.45), (0.55, 0.95));
    let (middle_dark, middle_bright) = (look(&photos[1], dark, (0.1, 0.9)), look(&photos[1], bright, (0.1, 0.9)));
    let result = fused(&photos, 0);
    let (got_dark, got_bright) = (look(&result, dark, (0.1, 0.9)), look(&result, bright, (0.1, 0.9)));
    // the middle photo has lost the dark part's texture to the dark and the bright part's to white: the merge has both back
    assert!(got_dark.0 > 1.5 * middle_dark.0, "dark: {got_dark:?} against {middle_dark:?}");
    assert!(got_bright.0 > 1.5 * middle_bright.0, "bright: {got_bright:?} against {middle_bright:?}");
}

#[test]
fn photos_that_moved_and_changed_size_a_little_are_lined_up_before_the_exposures_are_merged() {
    let shaky = |stops: f64, grow: f64, shift: (f64, f64)| {
        render(|x, y| {
            let (sx, sy) = (CENTRE.0 + (x - CENTRE.0 - shift.0) / (1.0 + grow), CENTRE.1 + (y - CENTRE.1 - shift.1) / (1.0 + grow));
            radiance(sx, sy).map(|v| (v * 2f64.powf(stops)).clamp(0.0, 1.0).powf(1.0 / 2.2))
        })
    };
    let photos = vec![shaky(-2.0, -0.01, (2.0, -1.5)), shaky(0.0, 0.0, (0.0, 0.0)), shaky(2.0, 0.012, (-1.0, 2.0))];
    let lined_up = fused(&photos, 0);
    let (dark, bright) = ((0.05, 0.45), (0.55, 0.95));
    let middle = look(&photos[1], dark, (0.1, 0.9));
    // blurred by a misalignment the texture would be less than the middle photo's
    assert!(look(&lined_up, dark, (0.1, 0.9)).0 > 1.3 * middle.0 && look(&lined_up, bright, (0.1, 0.9)).0 > 1.3 * look(&photos[1], bright, (0.1, 0.9)).0);
}

#[test]
fn something_that_moved_between_the_exposures_is_not_in_the_result_where_the_middle_photo_has_none() {
    // a green square is in the brightest photo only, where the others show the scene (in the dark half, where the middle photo has something to show)
    let clean = fused(&bracket(), 0);
    let ghosted = fused(&[exposure(-2.0, None), exposure(0.0, None), exposure(2.0, Some((160, 240)))], 0);
    let (a, b) = (look(&clean, (0.2, 0.3), (0.4, 0.6)), look(&ghosted, (0.2, 0.3), (0.4, 0.6)));
    let drift = (green(&clean, (0.2, 0.3), (0.4, 0.6)) - green(&ghosted, (0.2, 0.3), (0.4, 0.6))).abs();
    assert!(drift < 8.0, "green changed by {drift}: {a:?} against {b:?}");
    // the same square in the middle photo is the reference and does show
    let shown = fused(&[exposure(-2.0, None), exposure(0.0, Some((160, 240))), exposure(2.0, None)], 0);
    let c = look(&shown, (0.2, 0.3), (0.4, 0.6));
    assert!((green(&clean, (0.2, 0.3), (0.4, 0.6)) - green(&shown, (0.2, 0.3), (0.4, 0.6))).abs() > 20.0, "the reference photo's square: {c:?}");
}

#[test]
fn the_darker_to_lighter_setting_moves_the_merged_picture() {
    let photos = bracket();
    let darker = look(&fused(&photos, -100), (0.05, 0.95), (0.1, 0.9)).1;
    let middle = look(&fused(&photos, 0), (0.05, 0.95), (0.1, 0.9)).1;
    let lighter = look(&fused(&photos, 100), (0.05, 0.95), (0.1, 0.9)).1;
    assert!(darker + 4.0 < middle && middle + 4.0 < lighter, "{darker} {middle} {lighter}");
}

/// Not part of the suite: writes the three photos of a bracket and the merge as JPEGs into TELESCOPE_FOCUS_OUT, to look at.
/// `cargo nextest run -p telescope-effects --run-ignored only render_the_merges`
#[test]
#[ignore]
fn render_the_merges() {
    let Ok(out) = std::env::var("TELESCOPE_FOCUS_OUT") else { return };
    std::fs::create_dir_all(&out).unwrap();
    let photos = vec![photo(0, -0.02, (2.0, -1.0)), photo(1, 0.0, (0.0, 0.0)), photo(2, 0.025, (-1.5, 2.0))];
    for (i, p) in photos.iter().enumerate() {
        std::fs::write(format!("{out}/photo-{i}.jpg"), encode_jpeg(p, 92, None).unwrap()).unwrap();
    }
    let result = merged(photos);
    std::fs::write(format!("{out}/merged.jpg"), encode_jpeg(&result, 92, None).unwrap()).unwrap();
    std::fs::write(format!("{out}/truth.jpg"), encode_jpeg(&truth(), 92, None).unwrap()).unwrap();
    for (i, p) in bracket().iter().enumerate() {
        std::fs::write(format!("{out}/exposure-{i}.jpg"), encode_jpeg(p, 92, None).unwrap()).unwrap();
    }
    for (name, brightness) in [("darker", -100), ("middle", 0), ("lighter", 100)] {
        std::fs::write(format!("{out}/exposure-merged-{name}.jpg"), encode_jpeg(&fused(&bracket(), brightness), 92, None).unwrap()).unwrap();
    }
    let ghost = [exposure(-2.0, None), exposure(0.0, None), exposure(2.0, Some((160, 240)))];
    std::fs::write(format!("{out}/exposure-ghost-photo.jpg"), encode_jpeg(&ghost[2], 92, None).unwrap()).unwrap();
    std::fs::write(format!("{out}/exposure-ghost-merged.jpg"), encode_jpeg(&fused(&ghost, 0), 92, None).unwrap()).unwrap();
}
