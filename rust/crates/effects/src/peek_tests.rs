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

/// Between the shoulders and a hand's width down, which is the front of the sweater.
fn chest() -> (f64, f64) {
    let person = sample_person();
    let (l, r) = (person.point("left_shoulder", SEEN).unwrap(), person.point("right_shoulder", SEEN).unwrap());
    ((l.0 + r.0) / 2.0, (l.1 + r.1) / 2.0 + 0.06)
}

#[test]
fn the_framing_photo_is_one_the_framing_code_can_use() {
    let person = sample_person();
    for name in ["nose", "left_eye", "right_eye", "left_shoulder", "right_shoulder", "left_hip", "right_hip", "left_ankle", "right_ankle"] {
        assert!(person.point(name, SEEN).is_some(), "{name}");
    }
    let photo = scene();
    assert_eq!((photo.w, photo.h), (SCENE_WIDTH, SCENE_HEIGHT));
    assert!((photo.w as f64 / photo.h as f64 - SAMPLE_ASPECT).abs() < 1e-9, "the shape the framing code is told it has");
    // the person stands in the middle with the whole of them in the picture and some ground under the feet
    let (x, y) = chest();
    assert!((x - 0.5).abs() < 0.02, "{x}");
    let ankle = person.point("left_ankle", SEEN).unwrap().1.max(person.point("right_ankle", SEEN).unwrap().1);
    assert!(ankle < 0.9, "the feet are {ankle} of the way down");
    // the chest is on the dark green sweater, and the sky above the head is the light grey it is in the photo
    let [r, g, b] = photo.at((x * photo.w as f64) as usize, (y * photo.h as f64) as usize);
    assert!(g > r && g > b && (r as u32 + g as u32 + b as u32) < 240, "{r} {g} {b}");
    let sky = photo.at(photo.w / 2, 4);
    assert!(sky.iter().all(|c| *c > 110), "{sky:?}");
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
fn the_scene_picture_is_decoded_once_at_twice_the_bubbles_size() {
    let picture = scene_picture();
    assert_eq!((picture.width as usize, picture.height as usize), (SCENE_WIDTH, SCENE_HEIGHT));
    assert_eq!((SCENE_WIDTH, SCENE_HEIGHT), (2 * WIDTH, 2 * HEIGHT));
    assert!(std::ptr::eq(picture, scene_picture()), "kept, not decoded again");
    // the picture is the photo the framing code reads, opaque
    let (x, y) = chest();
    let (x, y) = ((x * SCENE_WIDTH as f64) as usize, (y * SCENE_HEIGHT as f64) as usize);
    let at = (y * SCENE_WIDTH + x) * 4;
    assert_eq!(&picture.data[at..at + 3], &scene().at(x, y));
    assert_eq!(picture.data[at + 3], 255);
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
