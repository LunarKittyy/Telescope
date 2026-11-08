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
fn a_crop_shows_only_what_the_window_covers() {
    let at = |u: f64, _v: f64| if u < 0.5 { [255, 0, 0] } else { [0, 0, 255] };
    let whole = crop(&at, (0.5, 0.5, 1.0), 8, 4);
    assert_eq!(&whole.data[0..4], &[255, 0, 0, 255]);
    assert_eq!(&whole.data[7 * 4..8 * 4], &[0, 0, 255, 255]);
    // a window over the left quarter is all red, over the right quarter all blue
    let left = crop(&at, (0.25, 0.5, 4.0), 8, 4);
    assert!(left.data.chunks(4).all(|p| p == [255, 0, 0, 255]));
    let right = crop(&at, (0.9, 0.5, 4.0), 8, 4);
    assert!(right.data.chunks(4).all(|p| p == [0, 0, 255, 255]));
}

#[test]
fn a_bubble_picture_fits_its_box_for_any_shape_of_source() {
    assert_eq!(size_for(16.0 / 9.0), (WIDTH, HEIGHT));
    assert_eq!(size_for(1.0), (HEIGHT, HEIGHT));
    let (w, h) = size_for(4.0);
    assert!(w <= WIDTH && h <= HEIGHT && w == WIDTH);
    assert_eq!(size_for(f64::NAN), (WIDTH, HEIGHT));
}
