use super::*;

fn jpeg(w: u32, h: u32, shade: u8) -> Vec<u8> {
    let rgb: Vec<u8> = (0..w * h).flat_map(|i| [shade, (i % 200) as u8, 40]).collect();
    let mut out = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95);
    image::ImageEncoder::write_image(encoder, &rgb, w, h, image::ExtendedColorType::Rgb8).unwrap();
    out
}

fn file(name: &str, bytes: &[u8]) -> std::path::PathBuf {
    let path = std::env::temp_dir().join(format!("stargazer-photos-{name}-{}.jpg", std::process::id()));
    std::fs::write(&path, bytes).unwrap();
    path
}

#[test]
fn a_photo_that_fits_the_budget_is_stacked_whole() {
    let plan = plan(5_000_000, (4000, 3000), 28, 2_000_000, 1_500_000_000).unwrap();
    assert_eq!((plan.factor, plan.size), (1, (4000, 3000)));
}

#[test]
fn a_bigger_photo_is_divided_until_it_fits_and_the_plan_says_the_size() {
    // 50 MP: 150 MB of RGB and 39 bytes a pixel is nearly 2 GB whole
    let source = (8160, 6120);
    let budget = 1_500_000_000;
    assert!(bytes_needed(20_000_000, source, 1, 28, 2_000_000) > budget);
    let plan = plan(20_000_000, source, 28, 2_000_000, budget).unwrap();
    assert_eq!(plan.factor, 2);
    assert_eq!(plan.size, (4080, 3060));
    assert!(bytes_needed(20_000_000, source, plan.factor, 28, 2_000_000) <= budget);
    assert!(bytes_needed(20_000_000, source, plan.factor - 1, 28, 2_000_000) > budget);
}

#[test]
fn the_mean_and_brightest_stacks_need_less_than_the_clipped_one() {
    let source = (6000, 4000);
    assert!(bytes_needed(0, source, 1, 16, 0) < bytes_needed(0, source, 1, 28, 0));
}

#[test]
fn a_photo_nothing_can_shrink_enough_is_refused_before_anything_is_allocated() {
    assert_eq!(plan(1_000_000, (20_000, 20_000), 28, 0, 100_000_000), None);
    assert_eq!(plan(0, (0, 100), 28, 0, 1_000_000_000), None);
    let path = std::env::temp_dir().join(format!("stargazer-photos-missing-{}.jpg", std::process::id()));
    assert!(read(&path).is_err());
}

#[test]
fn a_file_over_the_limit_is_never_read() {
    let path = file("huge", &[0u8; 16]);
    let big = std::fs::File::options().write(true).open(&path).unwrap();
    big.set_len(MAX_FILE_BYTES + 1).unwrap();
    assert!(read(&path).is_err());
    let _ = std::fs::remove_file(path);
}

#[test]
fn reading_gives_the_size_from_the_header_and_decoding_divides_it() {
    let path = file("size", &jpeg(64, 48, 200));
    let photo = read(&path).unwrap();
    assert_eq!(photo.size, (64, 48));
    assert!(!photo.is_empty() && photo.len() > 100);
    let frame = photo.decode(2).unwrap();
    assert_eq!((frame.width, frame.height, frame.format), (32, 24, PixelFormat::Rgba));
    assert!((i32::from(frame.data[0]) - 200).abs() <= 4);
    assert_eq!(frame.data[3], 255);
    let whole = read(&path).unwrap().decode(1).unwrap();
    assert_eq!((whole.width, whole.height), (64, 48));
    let _ = std::fs::remove_file(path);
}

#[test]
fn a_broken_file_is_an_error_and_not_a_panic() {
    let path = file("broken", b"not a photo at all");
    assert!(read(&path).is_err());
    let _ = std::fs::remove_file(path);
}

fn plain(w: u32, h: u32, rgb: [u8; 3]) -> Frame {
    Frame::new(w, h, PixelFormat::Rgba, (0..w * h).flat_map(|_| [rgb[0], rgb[1], rgb[2], 255]).collect())
}

#[test]
fn the_cameras_copy_is_the_videos_size_with_the_photos_shape_kept() {
    // A 4:3 stack on a 16:9 video: black bars left and right
    let copy = fit_into(&plain(400, 300, [200, 100, 50]), 192, 108).unwrap();
    assert_eq!((copy.width, copy.height, copy.format), (192, 108, PixelFormat::Rgba));
    let at = |x: usize, y: usize| &copy.data[(y * 192 + x) * 4..(y * 192 + x) * 4 + 4];
    assert_eq!(at(2, 54), &[0, 0, 0, 255]);
    assert_eq!(at(189, 54), &[0, 0, 0, 255]);
    assert_eq!(at(96, 54), &[200, 100, 50, 255]);
    // The picture is 144 wide, centred
    assert_eq!(at(24, 54), &[200, 100, 50, 255]);
    assert_eq!(at(22, 54), &[0, 0, 0, 255]);
}

#[test]
fn the_copy_is_made_for_any_size_and_refused_for_none() {
    assert!(fit_into(&plain(10, 10, [1, 2, 3]), 40, 20).is_some());
    assert!(fit_into(&plain(10, 10, [1, 2, 3]), 0, 20).is_none());
    let mut odd = plain(10, 10, [1, 2, 3]);
    odd.format = PixelFormat::Bgra;
    assert!(fit_into(&odd, 40, 20).is_none());
}
