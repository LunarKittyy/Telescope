use super::*;
use telescope_core::pose::SEEN;

#[test]
fn the_blur_sample_has_a_person_to_leave_out_and_a_wall_to_blur() {
    let (rgb, mask) = sample_scene(WIDTH, HEIGHT);
    assert_eq!((rgb.w, rgb.h, mask.w, mask.h), (WIDTH, HEIGHT, WIDTH, HEIGHT));
    assert_eq!((blur_scene().0.w, blur_scene().0.h), (BLUR_WIDTH, BLUR_HEIGHT));
    let inside = mask.data.iter().filter(|m| **m > 127).count();
    let share = inside as f64 / mask.data.len() as f64;
    assert!((0.2..0.6).contains(&share), "the person covers {share} of the picture");
    // the wall has detail to lose: neighbouring columns differ along a shelf, away from the person
    let busy = (0..60).filter(|x| rgb.at(*x, 30) != rgb.at(*x + 1, 30)).count();
    assert!(busy > 8, "only {busy} edges along a shelf");
    // and a smaller sample is the same picture shrunk
    let (small, small_mask) = sample_scene(WIDTH / 2, HEIGHT / 2);
    assert_eq!((small.w, small_mask.h), (WIDTH / 2, HEIGHT / 2));
}

#[test]
fn the_standing_scene_is_one_the_framing_code_can_use() {
    let person = sample_person();
    assert!(person.point("left_shoulder", SEEN).is_some() && person.point("nose", SEEN).is_some());
    let (rgb, _) = (scene().0.clone(), ());
    // the nose is on the skin tone of the drawn head and a point just under the shoulders' line is on the drawn shirt
    let nose = person.point("nose", SEEN).unwrap();
    assert_eq!(rgb.at((nose.0 * WIDTH as f64) as usize, (nose.1 * HEIGHT as f64) as usize), [224, 172, 140]);
    let shoulder = person.point("left_shoulder", SEEN).unwrap();
    assert_eq!(rgb.at(((shoulder.0 - 0.02) * WIDTH as f64) as usize, ((shoulder.1 + 0.05) * HEIGHT as f64) as usize), [52, 78, 140]);
}

#[test]
fn a_window_in_a_picture_that_already_shows_a_view_slides_in_and_never_asks_for_more_than_there_is() {
    // the view shows everything: the window is where it was asked for
    assert_eq!(window_in((0.5, 0.5, 1.0), (0.4, 0.45, 2.0)), (0.4, 0.45, 2.0));
    // the view is already 2x on the right half: a window the same size is the whole picture, one a quarter of it is a corner
    let view = (0.75, 0.5, 2.0);
    assert_eq!(window_in(view, (0.75, 0.5, 2.0)), (0.5, 0.5, 1.0));
    let (u, v, zoom) = window_in(view, (0.875, 0.5, 4.0));
    assert!((zoom - 2.0).abs() < 1e-9 && (u - 0.75).abs() < 1e-9 && (v - 0.5).abs() < 1e-9, "{u} {v} {zoom}");
    // wanting what is outside the view: the nearest the picture has
    let (u, _, _) = window_in(view, (0.1, 0.5, 4.0));
    assert!((u - 0.25).abs() < 1e-9, "{u}");
    // wider than the view shows: no more than the view
    assert_eq!(window_in(view, (0.5, 0.5, 1.0)).2, 1.0);
}

#[test]
fn a_clip_is_the_window_slid_in_to_stay_inside_the_picture() {
    assert_eq!(clip((0.5, 0.5, 1.0)), [0.0, 0.0, 1.0, 1.0]);
    assert_eq!(clip((0.5, 0.5, 2.0)), [0.25, 0.25, 0.5, 0.5]);
    // a window asked for past the left and top edges is held against them, and one past the right and bottom against those
    assert_eq!(clip((0.0, 0.0, 4.0)), [0.0, 0.0, 0.25, 0.25]);
    let [x, y, w, h] = clip((1.0, 1.0, 4.0));
    assert!((x - 0.75).abs() < 1e-9 && (y - 0.75).abs() < 1e-9 && (w - 0.25).abs() < 1e-9 && (h - 0.25).abs() < 1e-9);
    // never wider than the picture
    assert_eq!(clip((0.3, 0.7, 0.5))[2], 1.0);
}

#[test]
fn the_scene_picture_is_made_once_at_the_bubbles_size() {
    let picture = scene_picture();
    assert_eq!((picture.width as usize, picture.height as usize), (WIDTH, HEIGHT));
    assert!(std::ptr::eq(picture, scene_picture()), "kept, not drawn again");
    // the nose is on the skin tone, so what the bubble crops is the scene the framing code reads
    let nose = sample_person().point("nose", SEEN).unwrap();
    let at = ((nose.1 * HEIGHT as f64) as usize * WIDTH + (nose.0 * WIDTH as f64) as usize) * 4;
    assert_eq!(&picture.data[at..at + 4], &[224, 172, 140, 255]);
}

#[test]
fn a_live_frame_is_shrunk_for_the_bubble_and_never_enlarged() {
    let big = Frame::new(1920, 1080, PixelFormat::Rgba, vec![200; 1920 * 1080 * 4]);
    let shrunk = live_picture(&big).unwrap();
    assert_eq!((shrunk.width, shrunk.height), (2 * WIDTH as u32, 2 * HEIGHT as u32));
    assert!(shrunk.data.chunks(4).all(|p| p == [200, 200, 200, 255]));
    let small = Frame::new(320, 180, PixelFormat::Rgba, vec![10; 320 * 180 * 4]);
    let kept = live_picture(&small).unwrap();
    assert_eq!((kept.width, kept.height), (320, 180));
    // a tall one keeps its shape
    let tall = Frame::new(1080, 1920, PixelFormat::Rgba, vec![0; 1080 * 1920 * 4]);
    let shrunk = live_picture(&tall).unwrap();
    assert_eq!((shrunk.width as usize, shrunk.height as usize), (223, 2 * HEIGHT));
}
