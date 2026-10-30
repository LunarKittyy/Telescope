use super::*;

const W: usize = 160;
const H: usize = 90;

/// A room with a bit of texture, so differences from it are real ones.
fn room() -> Rgb {
    let mut r = Rgb::new(W, H);
    for y in 0..H {
        for x in 0..W {
            let b = (40.0 + 80.0 * x as f32 / (W - 1) as f32) as u8;
            let g = (60.0 + 40.0 * y as f32 / (H - 1) as f32) as u8;
            r.set(x, y, [70, g, b]);
        }
    }
    r
}

fn with_person(room: &Rgb, x0: usize, x1: usize, colour: [u8; 3]) -> Rgb {
    let mut f = room.clone();
    for y in 20..80 {
        for x in x0..x1 {
            f.set(x, y, colour);
        }
    }
    f
}

const PERSON: [u8; 3] = [240, 220, 20];

fn mask(x0: usize, x1: usize) -> Gray {
    let mut m = Gray::new(W, H);
    for y in 20..80 {
        for x in x0..x1 {
            m.data[y * W + x] = 255;
        }
    }
    m
}

fn run(look: &Look, frames: &[Rgb]) -> Vec<Rgb> {
    let mut e = Effect::named(&look.effect).expect("an effect");
    frames.iter().map(|f| e.run(f, look, Inputs { mask: None })).collect()
}

fn look(effect: &str) -> Look {
    Look { effect: effect.into(), ..Look::default() }
}

fn near(a: [u8; 3], b: [u8; 3], by: i32) -> bool {
    (0..3).all(|c| (a[c] as i32 - b[c] as i32).abs() <= by)
}

#[test]
fn the_empty_room_difference_finds_whoever_stands_in_front_of_it() {
    let room = room();
    let m = PlateMask::default().run(&with_person(&room, 60, 100, PERSON), &Arc::new(room)).unwrap();
    assert_eq!((m.data[50 * W + 80], m.data[50 * W + 10], m.data[5 * W + 80]), (255, 0, 0));
}

#[test]
fn the_cloak_shows_the_empty_room_where_you_are() {
    let room = room();
    let frame = with_person(&room, 60, 100, PERSON);
    let l = Look { plate: Some(Arc::new(room.clone())), edges: 0, ..look("cloak") };
    let out = &run(&l, std::slice::from_ref(&frame))[0];
    assert!(near(out.at(80, 50), room.at(80, 50), 10));
    assert_eq!(out.at(10, 50), frame.at(10, 50));
    // no empty room yet: left alone
    assert_eq!(run(&look("cloak"), std::slice::from_ref(&frame))[0], frame);
}

#[test]
fn the_cloak_uses_the_person_mask_when_the_vision_download_is_there() {
    let room = room();
    let frame = with_person(&room, 60, 100, PERSON);
    let l = Look { plate: Some(Arc::new(room.clone())), edges: 0, ..look("cloak") };
    let out = Effect::named("cloak").unwrap().run(&frame, &l, Inputs { mask: Some(mask(60, 100)) });
    assert!(near(out.at(80, 50), room.at(80, 50), 10));
}

#[test]
fn clones_stay_where_they_were_taken_and_you_stay_in_front() {
    let room = room();
    let first = with_person(&room, 10, 40, [255, 0, 0]);
    let clones = Clones::add(None, &first, &mask(10, 40));
    let second = with_person(&room, 60, 90, [0, 0, 255]);
    let clones = Clones::add(Some(&clones), &second, &mask(60, 90));
    assert_eq!(clones.count, 2);
    let now = with_person(&room, 70, 120, [0, 255, 0]);
    let l = Look { plate: Some(Arc::new(room.clone())), clones: Some(Arc::new(clones)), edges: 0, ..look("clones") };
    let out = &run(&l, &[now])[0];
    // the first clone, the second where you aren't, and you in front of it
    assert_eq!((out.at(20, 50), out.at(65, 50), out.at(80, 50)), ([255, 0, 0], [0, 0, 255], [0, 255, 0]));
    assert_eq!(out.at(5, 5), room.at(5, 5));
}

#[test]
fn light_painting_leaves_a_trail_that_fades_and_clear_starts_again() {
    let dark = Rgb::filled(W, H, [10, 10, 10]);
    let mut lit = dark.clone();
    for y in 40..44 {
        for x in 20..24 {
            lit.set(x, y, [255, 255, 255]);
        }
    }
    let l = Look { fade: 5, ..look("light") };
    let mut e = Effect::named("light").unwrap();
    let mut go = |f: &Rgb| e.run(f, &l, Inputs { mask: None });
    go(&lit);
    let mut after = go(&dark);
    // still there a frame later
    assert!(after.at(22, 42)[0] > 150);
    for _ in 0..60 {
        after = go(&dark);
    }
    // and gone after two seconds
    assert!(after.at(22, 42)[0] < 40);
    // Clear is a new effect, which has no trail
    let cleared = Effect::named("light").unwrap().run(&dark, &Look { reset: 1, ..l.clone() }, Inputs { mask: None });
    assert_eq!(cleared.at(22, 42)[0], 10);
}

#[test]
fn long_light_trails_still_fade_all_the_way() {
    let dark = Rgb::filled(32, 18, [5, 5, 5]);
    let mut lit = dark.clone();
    lit.set(10, 9, [255, 255, 255]);
    let l = Look { fade: 10, ..look("light") };
    let mut e = Effect::named("light").unwrap();
    e.run(&lit, &l, Inputs { mask: None });
    let mut after = dark.clone();
    for _ in 0..3000 {
        after = e.run(&dark, &l, Inputs { mask: None });
    }
    // rounding used to leave a ghost of the trail behind for good
    assert_eq!(after.at(10, 9)[0], 5);
}

#[test]
fn slit_scan_shows_the_bottom_of_the_picture_from_further_back() {
    let frames: Vec<Rgb> = (0..25).map(|i| Rgb::filled(W, H, [i as u8 * 10; 3])).collect();
    let out = run(&Look { spread: 20, ..look("slit") }, &frames).pop().unwrap();
    // the top: now, the bottom: 20 frames back
    assert_eq!(out.at(80, 1)[0], 240);
    assert!((out.at(80, H - 1)[0] as i32 - 40).abs() <= 5);
    let across = run(&Look { spread: 20, across: true, ..look("slit") }, &frames).pop().unwrap();
    assert!(across.at(0, 45)[0] == 240 && (across.at(W - 1, 45)[0] as i32 - 40).abs() <= 5);
}

#[test]
fn the_stick_figure_is_drawn_on_paper_or_the_empty_room() {
    let l = Look { lines: vec![((0.2, 0.2), (0.8, 0.8))], heads: vec![((0.5, 0.3), 0.1)], ..look("stick") };
    let out = &run(&l, &[room()])[0];
    assert_eq!((out.at(W - 3, 2), out.at(80, 45)), (PAPER, INK));
    let r = room();
    let on_room = &run(&Look { plate: Some(Arc::new(r.clone())), ..look("stick") }, &[with_person(&r, 60, 100, PERSON)])[0];
    // nobody seen: just the room
    assert_eq!(*on_room, r);
}

fn big(frame: &Rgb) -> Rgb {
    frame.nearest(frame.w * 4, frame.h * 4)
}

#[test]
fn each_retro_look_keeps_the_size_and_changes_the_picture() {
    let frame = big(&with_person(&room(), 60, 100, PERSON));
    for effect in ["webcam", "vhs", "crt", "ascii"] {
        let out = run(&look(effect), &[frame.clone(), frame.clone()]).pop().unwrap();
        assert_eq!((out.w, out.h), (frame.w, frame.h), "{effect}");
        let mean = out.data.iter().zip(&frame.data).map(|(&a, &b)| (a as f64 - b as f64).abs()).sum::<f64>() / out.data.len() as f64;
        assert!(mean > 5.0, "{effect} changed the picture by {mean}");
    }
}

#[test]
fn the_1998_webcam_repeats_every_other_frame_without_handing_out_the_same_one() {
    let frame = big(&with_person(&room(), 60, 100, PERSON));
    let mut cam = Webcam::new();
    let (first, second, third) = (cam.run(&frame), cam.run(&frame), cam.run(&frame));
    assert!(first.data.iter().any(|&v| v != 0));
    assert_eq!(third.w, frame.w);
    // each new picture has its own noise
    assert_ne!(first, second);
    assert_eq!(cam.run(&frame).w, frame.w);
}

#[test]
fn a_jpeg_at_low_quality_blocks_the_picture_but_keeps_its_colours() {
    let mut img = Rgb::new(32, 32);
    for y in 0..32 {
        for x in 0..32 {
            img.set(x, y, [(x * 8) as u8, (y * 8) as u8, 128]);
        }
    }
    let out = jpeg(&img, 12);
    let mean = out.data.iter().zip(&img.data).map(|(&a, &b)| (a as f64 - b as f64).abs()).sum::<f64>() / out.data.len() as f64;
    assert!(mean > 1.0 && mean < 30.0, "{mean}");
    assert!(near(jpeg(&Rgb::filled(16, 16, [200, 60, 90]), 12).at(8, 8), [200, 60, 90], 10));
}

#[test]
fn the_ascii_picture_is_green_text_on_black() {
    let out = ascii(&big(&with_person(&room(), 60, 100, PERSON)));
    assert!(out.data.chunks(3).all(|p| p == [0, 0, 0] || p == [0, 255, 85]));
    assert!(out.data.chunks(3).any(|p| p == [0, 255, 85]));
}

#[test]
fn a_plate_of_another_shape_is_never_stretched_over_the_picture() {
    let room = room();
    let turned = Rgb { w: H, h: W, data: with_person(&room, 60, 100, PERSON).data[..H * W * 3].to_vec() };
    let l = Look { plate: Some(Arc::new(room)), edges: 0, ..look("cloak") };
    assert_eq!(run(&l, std::slice::from_ref(&turned))[0], turned);
}

#[test]
fn the_crt_goes_black_outside_its_bulge_and_dims_the_corners() {
    let out = run(&look("crt"), &[Rgb::filled(320, 180, [200, 200, 200])]).pop().unwrap();
    assert!(out.at(160, 90)[1] > 100);
    assert!(out.at(0, 0).iter().all(|&v| v < 40));
}

#[test]
fn another_stream_starts_without_the_last_ones_history() {
    let mut e = Effect::named("slit").unwrap();
    let l = Look { spread: 20, ..look("slit") };
    for _ in 0..30 {
        e.run(&Rgb::filled(W, H, [200; 3]), &l, Inputs { mask: None });
    }
    let dark = Rgb::filled(W, H, [10; 3]);
    let fresh = Effect::named("slit").unwrap().run(&dark, &l, Inputs { mask: None });
    assert!(fresh.data.iter().all(|&v| v < 50));
}

#[test]
fn the_cloak_compares_with_the_room_it_has_now_not_one_that_was_there_before() {
    let (a, b) = (room(), Rgb::filled(W, H, [200, 30, 90]));
    let mut cloak = Effect::named("cloak").unwrap();
    let (first, second) = (Look { plate: Some(Arc::new(a.clone())), edges: 0, ..look("cloak") }, Look { plate: Some(Arc::new(b.clone())), edges: 0, ..look("cloak") });
    cloak.run(&a, &first, Inputs { mask: None });
    // a new room is taken; the frame is it, a little off but nobody in it
    let mut frame = b;
    frame.set(5, 5, [205, 35, 95]);
    assert!(cloak.run(&frame, &second, Inputs { mask: None }) == frame, "nobody there, so the frame is left as it is");
}

#[test]
fn the_webcams_height_rounds_halves_to_even_as_pythons_round_does() {
    // 481 * 320 / 640 = 240.5: Python's round() gives 240, and 483 gives 241.5 and so 242
    let mut cam = Webcam::new();
    cam.run(&Rgb::filled(640, 481, [90; 3]));
    let (small, _) = cam.held.as_ref().unwrap();
    assert_eq!((small.w, small.h), (320, 240));
    let mut cam = Webcam::new();
    cam.run(&Rgb::filled(640, 483, [90; 3]));
    assert_eq!(cam.held.as_ref().unwrap().0.h, 242);
}
