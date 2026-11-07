//! Auto colour grading's measuring and fit on generated scenes: known wrong grades put on a right-looking picture, and stability, fallbacks and determinism.

use crate::auto_fit::{decide, fit, skin_angle, Basic, Fit, Wb};
use crate::auto_measure::{select_skin, FaceMeasure, Grey, Look, Measurer, Pix};
use crate::auto_scene::*;
use crate::auto_tuning::*;
use crate::cpu::Rgb;
use crate::grade::Grade;
use telescope_core::view::Geometry;

type Camera = Box<dyn Fn(&[f32]) -> Rgb>;

fn warm() -> Camera {
    cast([1.15, 1.0, 0.85])
}

fn cool() -> Camera {
    cast([0.87, 1.0, 1.15])
}

fn green() -> Camera {
    cast([0.93, 1.14, 0.93])
}

fn magenta() -> Camera {
    cast([1.27, 0.62, 1.27])
}

fn dark() -> Camera {
    graded(Grade { masters: [0.0, -0.8, 0.4, 0.0], ..Grade::default() })
}

fn bright() -> Camera {
    graded(Grade { masters: [0.0, 0.5, 1.1, 0.0], ..Grade::default() })
}

fn flat() -> Camera {
    graded(Grade { contrast: 0.55, masters: [0.12, 0.0, 1.0, 0.0], ..Grade::default() })
}

fn right() -> Camera {
    graded(Grade::default())
}

struct Recovery {
    fit: Fit,
    face: (f32, f32),
    picture: (f32, f32),
}

/// How far the picture and the face are from the right-looking one before and after Auto, for a scene seen through a wrong camera.
fn recover(scene: &Scene, wrong: &Camera) -> Recovery {
    let original = observed(&scene.render(99), &Grade::default());
    let bad = wrong(&scene.render(99));
    let fit = fit(&measure(scene, wrong.as_ref()), None);
    let seen: Vec<f32> = bad.data.iter().map(|b| *b as f32 / 255.0).collect();
    let fixed = observed(&seen, &fit.values.grade());
    Recovery {
        fit,
        face: (difference(&bad, &original, Scene::in_face), difference(&fixed, &original, Scene::in_face)),
        picture: (difference(&bad, &original, |_, _| true), difference(&fixed, &original, |_, _| true)),
    }
}

fn report(name: &str, r: &Recovery) {
    println!("{name}: face {:.3} -> {:.3}, picture {:.3} -> {:.3}, {:?} {:?}", r.face.0, r.face.1, r.picture.0, r.picture.1, r.fit.values, r.fit.wb);
}

fn of(scene: &Scene, camera: &Camera) -> Fit {
    fit(&measure(scene, camera.as_ref()), None)
}

#[test]
fn the_default_scene_reads_as_a_face_and_grey_walls() {
    let m = measure(&Scene::default(), right().as_ref());
    assert!(!m.is_empty(), "{} frames", m.frames);
    let face = m.face.expect("a face");
    assert!((face.colour[0] - 0.76).abs() < 0.06 && (face.colour[2] - 0.45).abs() < 0.06, "{:?}", face.colour);
    assert!(matches!(m.grey, Grey::Agree { .. }), "{:?}", m.grey);
    assert!(!m.skin.is_empty() && !m.all.is_empty() && !m.background.is_empty());
    assert!(m.numbers()["face"]["skin_angle"].is_number());
}

#[test]
fn each_known_wrong_grade_lands_closer_to_the_original() {
    let cases: [(&str, Camera, Scene); 7] = [
        ("green", green(), Scene::default()),
        ("magenta", magenta(), Scene::default()),
        ("dark", dark(), Scene::default()),
        ("bright", bright(), Scene { skin: PALE, ..Scene::default() }),
        ("flat", flat(), Scene::default()),
        // no face to go by: the grey walls give the warm and cool casts away
        ("warm without a face", warm(), Scene { person: false, ..Scene::default() }),
        ("cool without a face", cool(), Scene { person: false, ..Scene::default() }),
    ];
    for (name, wrong, scene) in cases {
        let r = recover(&scene, &wrong);
        report(name, &r);
        assert!(r.fit.moved.any(), "{name} changed nothing");
        if name == "flat" {
            // lift and contrast only go so far, so the numbers are judged on what they were asked to do
            assert!(r.fit.values.contrast > 1.0 && r.fit.values.lift < 0.0 && r.face.1 <= r.face.0 + 0.04, "{name}: {:?}", r.face);
        } else {
            assert!(r.picture.1 < r.picture.0 * 0.95, "{name}: picture {:?}", r.picture);
        }
    }
}

#[test]
fn a_warm_or_cool_cast_that_keeps_the_face_on_the_skin_line_is_left_alone() {
    // skin only gives a hue line, and warm and cool light slide along it: the face can't tell
    for wrong in [warm(), cool()] {
        let r = recover(&Scene::default(), &wrong);
        assert_eq!(r.fit.wb, Wb::InRange);
        assert!(r.picture.1 <= r.picture.0 + 0.005, "{:?}", r.picture);
    }
}

#[test]
fn skin_tones_from_pale_to_dark_are_recovered_the_same_way() {
    for (name, skin) in [("pale", PALE), ("mid", MID), ("dark", DARK)] {
        let scene = Scene { skin, ..Scene::default() };
        for (kind, wrong) in [("green", green()), ("magenta", magenta())] {
            let r = recover(&scene, &wrong);
            report(&format!("{name} {kind}"), &r);
            // a face the correction leaves on the skin line already is left alone, and then nothing gets worse
            assert!(r.face.1 <= r.face.0 + 0.02, "{name} {kind}: face {:?}", r.face);
        }
        let fine = of(&scene, &right());
        assert!(!fine.moved.any(), "{name}: {:?}", fine.values);
    }
}

#[test]
fn a_picture_that_already_looks_right_barely_moves() {
    for scene in [Scene::default(), Scene { skin: PALE, ..Scene::default() }, Scene { skin: DARK, ..Scene::default() }] {
        let f = of(&scene, &right());
        assert_eq!(f.values, Basic::default(), "{:?}", f.values);
        assert!(!f.moved.any());
    }
}

#[test]
fn the_same_scene_a_few_seconds_later_gives_the_same_answer() {
    for wrong in [green, dark, flat, right] {
        let scene = Scene::default();
        let (a, b) = (fit(&measure(&scene, wrong().as_ref()), None).values, fit(&measure_from(&scene, wrong().as_ref(), 500), None).values);
        println!("later: {a:?} and {b:?}");
        assert!((a.temperature - b.temperature).abs() < 3.0 && (a.tint - b.tint).abs() < 3.0, "{a:?} {b:?}");
        assert!((a.gamma - b.gamma).abs() < 0.04 && (a.gain - b.gain).abs() < 0.04 && (a.contrast - b.contrast).abs() < 0.05 && (a.saturation - b.saturation).abs() < 0.05, "{a:?} {b:?}");
    }
}

#[test]
fn the_same_frames_always_give_the_same_answer() {
    let (a, b) = (measure(&Scene::default(), warm().as_ref()), measure(&Scene::default(), warm().as_ref()));
    assert_eq!(fit(&a, None), fit(&a, None));
    assert_eq!(fit(&a, None), fit(&b, None));
}

#[test]
fn with_no_person_it_goes_by_the_grey_walls_and_the_levels() {
    let scene = Scene { person: false, ..Scene::default() };
    let m = measure(&scene, warm().as_ref());
    assert!(m.face.is_none());
    let f = fit(&m, None);
    println!("no face: {:?} {:?}", f.values, f.wb);
    assert!(!f.had_face);
    assert_eq!(f.wb, Wb::GreyOnly);
    assert!(f.values.temperature < -3.0, "a warm picture is cooled: {:?}", f.values);
    assert!(f.values.temperature >= -WB_MAX_TEMPERATURE);
}

#[test]
fn a_face_with_nothing_else_to_go_by_is_corrected_from_the_face_alone() {
    // a wall too coloured to count as grey
    let scene = Scene { wall: [0.2, 0.55, 0.3], wall_right: [0.2, 0.55, 0.3], ..Scene::default() };
    let m = measure(&scene, cast([0.85, 1.3, 0.85]).as_ref());
    assert!(matches!(m.grey, Grey::None), "{:?}", m.grey);
    let f = fit(&m, None);
    assert_eq!(f.wb, Wb::Corrected, "{:?}", f.values);
    assert!(f.values.tint > 0.0, "green is countered with magenta: {:?}", f.values);
}

#[test]
fn conflicting_light_leaves_white_balance_alone() {
    let scene = Scene { wall: [0.73, 0.64, 0.64], wall_right: [0.64, 0.64, 0.73], ..Scene::default() };
    let m = measure(&scene, green().as_ref());
    assert!(matches!(m.grey, Grey::Conflict), "{:?}", m.grey);
    assert!(m.face.is_some_and(|f| crate::auto_fit::angle_excess(f.colour) > 0.0), "the face is off, so it matters");
    let f = fit(&m, None);
    assert_eq!(f.wb, Wb::Conflict, "{:?}", f.values);
    assert_eq!((f.values.temperature, f.values.tint), (0.0, 0.0));
}

#[test]
fn a_face_inside_the_skin_range_still_has_a_plainly_yellow_room_corrected() {
    let scene = Scene { wall: [0.72, 0.64, 0.52], wall_right: [0.72, 0.64, 0.52], ..Scene::default() };
    let f = of(&scene, &right());
    // a yellow wall round a fine face is most likely a yellow lamp, and is corrected
    assert_eq!(f.wb, Wb::World, "{:?}", f.wb);
    assert!(f.values.temperature < -20.0, "{:?}", f.values);
}

#[test]
fn a_correction_only_goes_part_of_the_way() {
    let f = of(&Scene::default(), &cast([0.8, 1.45, 0.8]));
    assert!(f.values.tint.abs() <= WB_MAX_TINT + 1e-9);
    assert!(f.values.tint > 0.0, "{:?}", f.values);
}

#[test]
fn a_dim_face_is_lifted_but_a_darker_skin_tone_that_is_well_lit_is_left() {
    let dim = of(&Scene::default(), &dark());
    assert!(dim.targets.face_luma.is_some() && dim.moved.exposure, "{:?}", dim.values);
    assert!(dim.values.gamma > 0.0 || dim.values.gain > 1.0);
    let dark_skin = of(&Scene { skin: DARK, ..Scene::default() }, &right());
    assert!(dark_skin.targets.face_luma.is_none() && !dark_skin.moved.exposure, "{:?}", dark_skin.values);
}

#[test]
fn a_clipped_face_is_pulled_down() {
    let f = of(&Scene { skin: PALE, ..Scene::default() }, &bright());
    println!("bright pale: {:?}", f.values);
    assert!(f.targets.face_luma.is_some());
    assert!(f.values.gamma < 0.0 || f.values.gain < 1.0, "{:?}", f.values);
}

#[test]
fn clipped_windows_are_left_out_of_the_targets_and_not_chased() {
    let with = Scene { window: true, ..Scene::default() };
    let m = measure(&with, right().as_ref());
    assert!(m.tone.white_clipped > 0.02, "{:?}", m.tone);
    assert!(m.all.iter().any(|p| p.clipped));
    let f = fit(&m, None);
    assert!(decide(&m).near_clip.is_none() && f.values.gain <= 1.0 + 1e-9, "{:?}", f.values);
    assert!(!f.moved.any(), "{:?}", f.values);
}

#[test]
fn a_flat_picture_gets_contrast_and_a_punchy_one_none() {
    let f = of(&Scene::default(), &flat());
    assert!(f.values.contrast > 1.0, "{:?}", f.values);
    let punchy = graded(Grade { contrast: 1.6, ..Grade::default() });
    let p = of(&Scene::default(), &punchy);
    assert!(p.values.contrast <= 1.0 + 1e-9, "{:?}", p.values);
}

#[test]
fn pivot_stays_at_its_default_without_contrast_and_goes_near_the_face_with_it() {
    assert_eq!(of(&Scene::default(), &right()).values.pivot, crate::grade::PIVOT);
    let f = of(&Scene::default(), &flat());
    if f.values.contrast > 1.0 {
        assert!((PIVOT_RANGE.0..=PIVOT_RANGE.1).contains(&f.values.pivot));
    }
}

#[test]
fn a_person_walking_through_is_skipped_as_motion() {
    let scene = Scene::default();
    let (people, view, mask) = (scene.people(), Geometry::default(), scene.mask().shrink(160, 90));
    let mut m = Measurer::new();
    for k in 0..20u32 {
        let mut pixels = scene.render(k + 1);
        if k % 4 == 1 {
            // a bright block jumps into the frame
            for y in 20..160 {
                for x in 40..120 {
                    pixels[(y * W + x) * 3..][..3].copy_from_slice(&[1.0, 1.0, 1.0]);
                }
            }
        }
        let small = observed(&pixels, &Grade::default()).shrink(160, 90);
        m.look(&Look { small: &small, mask: Some(&mask), people: &people, main: None, view: &view, noise: None, time_ns: k as u64 * 100_000_000 });
    }
    let measured = m.finish();
    assert!(measured.moving >= 5, "{}", measured.moving);
    assert!(measured.face.is_some() && !measured.is_empty());
}

#[test]
fn old_landmarks_and_a_missing_mask_leave_no_face() {
    let scene = Scene::default();
    let (view, mask) = (Geometry::default(), scene.mask().shrink(160, 90));
    let mut m = Measurer::new();
    for k in 0..14u32 {
        let small = observed(&scene.render(k + 1), &Grade::default()).shrink(160, 90);
        // landmarks that never came (stale ones are never handed over) and a mask that says nobody's there
        let nobody = crate::imgops::Gray::new(mask.w, mask.h);
        m.look(&Look { small: &small, mask: Some(&nobody), people: &scene.people(), main: None, view: &view, noise: None, time_ns: k as u64 * 100_000_000 });
    }
    assert!(m.finish().face.is_none(), "a mask that doesn't cover the face discards it");
}

#[test]
fn glare_makeup_and_beards_make_a_face_uncertain() {
    // half the face a quite different colour
    let skin: Vec<[f32; 3]> = (0..400).map(|i| if i % 2 == 0 { [0.76, 0.57, 0.45] } else { [0.45, 0.62, 0.40] }).collect();
    assert!(select_skin(&skin).is_none());
    let plain: Vec<[f32; 3]> = (0..400).map(|i| [0.76 + (i % 7) as f32 * 0.002, 0.57, 0.45]).collect();
    assert!(select_skin(&plain).is_some());
    let few: Vec<[f32; 3]> = (0..20).map(|_| [0.76, 0.57, 0.45]).collect();
    assert!(select_skin(&few).is_none());
}

#[test]
fn skin_sits_near_one_line_on_the_vectorscope_for_every_tone() {
    for skin in [PALE, MID, DARK] {
        let d = (skin_angle(skin) - 123.0).abs();
        assert!(d < SKIN_BAND_DEG, "{skin:?}: {d}");
    }
}

#[test]
fn saturation_is_capped_and_a_dull_picture_gets_some_colour() {
    let dull = graded(Grade { saturation: 0.3, ..Grade::default() });
    let f = of(&Scene { skin: PALE, ..Scene::default() }, &dull);
    println!("dull: {:?}", f.values);
    assert!(f.values.saturation > 1.0 && f.values.saturation <= SATURATION_RANGE.1 + 1e-9);
}

#[test]
fn the_sliders_it_sets_stay_inside_their_caps() {
    for wrong in [warm(), cool(), green(), dark(), bright(), flat()] {
        let v = of(&Scene::default(), &wrong).values;
        assert!(v.temperature.abs() <= WB_MAX_TEMPERATURE && v.tint.abs() <= WB_MAX_TINT);
        assert!((GAMMA_RANGE.0..=GAMMA_RANGE.1).contains(&v.gamma) && (GAIN_RANGE.0..=GAIN_RANGE.1).contains(&v.gain));
        assert!((LIFT_RANGE.0..=LIFT_RANGE.1).contains(&v.lift) && (CONTRAST_RANGE.0..=CONTRAST_RANGE.1).contains(&v.contrast));
        assert!((HIGHLIGHTS_RANGE.0..=HIGHLIGHTS_RANGE.1).contains(&v.highlights) && (SATURATION_RANGE.0..=SATURATION_RANGE.1).contains(&v.saturation));
    }
}

#[test]
fn the_persons_own_look_is_checked_so_it_cant_be_clipped_further() {
    // a backlit person: a dim face against a mid-bright room, where Auto lifting the exposure is fine alone but clips on top of their own offset
    let mut m = measure(&Scene::default(), right().as_ref());
    let dim = [0.22, 0.15, 0.12];
    m.tone.p50 = 0.2;
    m.skin = vec![dim; 500];
    m.face = Some(FaceMeasure { colour: dim, luma: crate::auto_measure::luma(dim), clipped: 0.0, spread: 0.02, frames: 10 });
    m.all = (0..600).map(|i| Pix { rgb: [0.6 + (i % 5) as f32 * 0.01; 3], clipped: false }).collect();
    m.background = m.all.clone();
    let own = Grade { masters: [0.0, 0.0, 1.0, 0.3], ..Grade::default() };
    let alone = fit(&m, None);
    let f = fit(&m, Some(&own));
    println!("backed off {} {:?} vs {:?}", f.backed_off, f.values, alone.values);
    assert!(alone.moved.exposure && !alone.backed_off);
    assert!(f.backed_off && (f.values.gamma < alone.values.gamma || f.values.gain < alone.values.gain));
}

#[test]
fn well_lit_dark_skin_is_not_taken_for_underexposure() {
    // [0.24, 0.16, 0.12] measures 0.17, under the face limit, in a room that is plainly bright
    let scene = Scene { skin: [0.24, 0.16, 0.12], ..Scene::default() };
    let f = of(&scene, &right());
    assert!(f.targets.face_luma.is_none() && !f.moved.exposure, "{:?}", f.values);
}

#[test]
fn a_flat_one_colour_room_stays_as_it_is() {
    let wall: Camera = Box::new(|_| Rgb::filled(W, H, [51, 51, 51]));
    let f = of(&Scene { person: false, ..Scene::default() }, &wall);
    assert_eq!(f.values, Basic::default(), "{:?}", f.values);
    assert!(f.targets.black.is_none() && f.targets.spread.is_none());
}

#[test]
fn white_balance_left_alone_keeps_the_persons_own_not_neutral() {
    let scene = Scene { wall: [0.73, 0.64, 0.64], wall_right: [0.64, 0.64, 0.73], ..Scene::default() };
    let mut m = measure(&scene, green().as_ref());
    m.tone.p50 = 0.2;
    let own = Grade { temperature: 12.0, tint: -7.0, ..Grade::default() };
    let f = fit(&m, Some(&own));
    assert_eq!(f.wb, Wb::Conflict);
    assert_eq!((f.values.temperature, f.values.tint), (12.0, -7.0));
    assert!(!f.moved.white_balance, "{:?}", f.moved);
}

#[test]
fn a_matrix_of_exposures_casts_and_haze_never_gets_worse_and_the_big_ones_move_clearly_towards_the_original() {
    let by = |ev: f32, g: [f32; 3]| -> Camera {
        let k = 2f32.powf(ev);
        cast([g[0] * k, g[1] * k, g[2] * k])
    };
    // (name, camera, must move clearly)
    let cases: Vec<(&str, Camera, bool)> = vec![
        ("-2 EV", by(-2.0, [1.0; 3]), true),
        ("-1 EV", by(-1.0, [1.0; 3]), true),
        ("+1 EV", by(1.0, [1.0; 3]), true),
        ("+2 EV", by(2.0, [1.0; 3]), false),
        ("tungsten", by(0.0, [1.25, 1.0, 0.65]), false),
        ("cool", by(0.0, [0.88, 1.0, 1.18]), false),
        ("green fluorescent", by(0.0, [0.88, 1.15, 0.88]), false),
        ("hazy", flat(), false),
        ("dark and warm", by(-1.5, [1.25, 1.0, 0.65]), true),
        ("bright and cool", by(1.5, [0.88, 1.0, 1.18]), false),
    ];
    for person in [true, false] {
        let scene = Scene { person, ..Scene::default() };
        for (name, camera, clear) in &cases {
            let r = recover(&scene, camera);
            report(&format!("{name} (person {person})"), &r);
            assert!(r.picture.1 <= r.picture.0 * 1.05 + 0.04, "{name} (person {person}) gets worse: {:?}", r.picture);
            if person {
                assert!(r.face.1 <= r.face.0 * 1.05 + 0.03, "{name} face gets worse: {:?}", r.face);
            }
            if *clear {
                assert!(r.picture.1 < r.picture.0 * 0.85, "{name} (person {person}) doesn't move clearly: {:?}", r.picture);
            }
        }
    }
}

#[test]
fn a_fine_face_does_not_let_conflicting_walls_or_a_biased_average_recolour_the_picture() {
    let scene = Scene { wall: [0.80, 0.64, 0.45], wall_right: [0.45, 0.64, 0.80], ..Scene::default() };
    let f = of(&scene, &right());
    assert_eq!(f.values.temperature, 0.0, "{:?}", f.values);
    assert_ne!(f.wb, Wb::World);
}
