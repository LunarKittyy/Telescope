use super::*;

const ASPECT: f64 = 16.0 / 9.0;
/// How often the pose model runs while it does the work alone (power saving's pace).
const GAP: f64 = 0.16;

fn face_at(cx: f64) -> Face {
    Face { area: (cx - 0.05, 0.2, cx + 0.05, 0.38), right_eye: (cx - 0.02, 0.27), left_eye: (cx + 0.02, 0.27), nose: (cx, 0.3), right_ear: (cx - 0.04, 0.28), left_ear: (cx + 0.04, 0.28), score: 0.9 }
}

/// A body whose shoulders are `width` face widths apart, `drop` face widths under the eyes of face_at(cx).
fn body_under(cx: f64, width: f64, drop: f64) -> Person {
    let face_w = 0.1 * ASPECT;
    let (half, sy) = (width * face_w / 2.0 / ASPECT, 0.27 + drop * face_w);
    Person::new([("left_eye", cx + 0.02, 0.27, 0.9), ("right_eye", cx - 0.02, 0.27, 0.9), ("left_shoulder", cx + half, sy, 0.9), ("right_shoulder", cx - half, sy, 0.9)])
}

/// Runs `people` with a pose model that finds `bodies` and counts its runs; None when nothing is new.
fn maybe_people(heads: &mut Heads, faces: &[Face], now: f64, bodies: &[Person], runs: &mut u32) -> Option<Vec<Person>> {
    heads.people(faces, ASPECT, now, GAP, &mut || {
        *runs += 1;
        Ok(bodies.to_vec())
    }).unwrap()
}

fn people_with(heads: &mut Heads, faces: &[Face], now: f64, bodies: &[Person], runs: &mut u32) -> Vec<Person> {
    maybe_people(heads, faces, now, bodies, runs).expect("people on this frame")
}

#[test]
fn a_face_becomes_a_person_with_shoulders_under_it() {
    let mut heads = Heads::new();
    let mut runs = 0;
    let people = people_with(&mut heads, &[face_at(0.5)], 0.0, &[], &mut runs);
    assert_eq!(runs, 1, "the first frame checks the body");
    let (l, r) = (people[0].point("left_shoulder", SEEN).unwrap(), people[0].point("right_shoulder", SEEN).unwrap());
    assert!(l.0 > 0.5 && r.0 < 0.5, "the person's left is on the right of the picture");
    let width = (l.0 - r.0) * ASPECT / (0.1 * ASPECT);
    assert!((width - SHOULDERS_PER_FACE).abs() < 1e-9, "{width}");
    assert!((l.1 - (0.27 + SHOULDERS_UNDER_EYES * 0.1 * ASPECT)).abs() < 1e-9);
    assert!(people[0].point("nose", SEEN).is_some());
}

#[test]
fn the_body_check_teaches_how_this_head_sits_on_its_shoulders() {
    let mut heads = Heads::new();
    let mut runs = 0;
    let body = body_under(0.5, 2.2, 1.0);
    for i in 0..10 {
        people_with(&mut heads, &[face_at(0.5)], i as f64 * BODY_CHECK_S, std::slice::from_ref(&body), &mut runs);
    }
    assert_eq!(runs, 10, "once a second");
    let (shoulders, drop) = heads.shape();
    assert!((shoulders - 2.2).abs() < 0.01 && (drop - 1.0).abs() < 0.01, "{shoulders} {drop}");
    // Learned shoulders follow the face between checks
    let people = people_with(&mut heads, &[face_at(0.3)], 9.5, &[], &mut runs);
    let (l, r) = (people[0].point("left_shoulder", SEEN).unwrap(), people[0].point("right_shoulder", SEEN).unwrap());
    assert!(((l.0 + r.0) / 2.0 - 0.3).abs() < 1e-9);
    assert!(((l.0 - r.0) / 0.1 - 2.2).abs() < 0.01);
}

#[test]
fn between_checks_only_the_face_finder_runs() {
    let mut heads = Heads::new();
    let mut runs = 0;
    for i in 0..12 {
        people_with(&mut heads, &[face_at(0.5)], i as f64 * 0.08, &[body_under(0.5, 1.7, 1.3)], &mut runs);
    }
    // 0 to 0.88 s: under a second, so one check
    assert_eq!(runs, 1);
    people_with(&mut heads, &[face_at(0.5)], 1.0, &[], &mut runs);
    assert_eq!(runs, 2);
}

#[test]
fn more_bodies_than_faces_is_a_group_and_the_pose_model_takes_over_at_its_own_pace() {
    let mut heads = Heads::new();
    let mut runs = 0;
    let two = [body_under(0.3, 1.7, 1.3), body_under(0.7, 1.7, 1.3)];
    let people = people_with(&mut heads, &[face_at(0.3)], 0.0, &two, &mut runs);
    assert_eq!(people, two, "the bodies, not the one face");
    let later: Vec<Option<Vec<Person>>> = (1..5).map(|i| maybe_people(&mut heads, &[face_at(0.3)], i as f64 * 0.08, &two, &mut runs)).collect();
    assert_eq!(later, [None, Some(two.to_vec()), None, Some(two.to_vec())], "every pose gap, with nothing new in between");
    assert_eq!(runs, 3);
    // Once the pose model sees no more people than faces, faces do it again
    let one = [body_under(0.3, 1.7, 1.3)];
    people_with(&mut heads, &[face_at(0.3)], 0.5, &one, &mut runs);
    let people = people_with(&mut heads, &[face_at(0.3)], 0.58, &one, &mut runs);
    assert_eq!(runs, 4);
    assert_ne!(people, one);
}

#[test]
fn a_face_missed_for_a_moment_keeps_its_person_then_the_body_takes_over() {
    let mut heads = Heads::new();
    let mut runs = 0;
    let body = [body_under(0.5, 1.7, 1.3)];
    let seen = people_with(&mut heads, &[face_at(0.5)], 0.0, &body, &mut runs);
    assert_eq!(people_with(&mut heads, &[], 0.1, &body, &mut runs), seen, "a turn of the head");
    assert_eq!(runs, 1);
    // Gone for FACE_LOST_S: the pose model does it alone, every pose gap
    assert_eq!(maybe_people(&mut heads, &[], FACE_LOST_S, &body, &mut runs), Some(body.to_vec()));
    assert_eq!(maybe_people(&mut heads, &[], FACE_LOST_S + 0.08, &body, &mut runs), None);
    assert_eq!(maybe_people(&mut heads, &[], 0.5, &body, &mut runs), Some(body.to_vec()));
    assert_eq!(runs, 3);
    // A face back: faces again at once, and the body waits for its check
    let back = people_with(&mut heads, &[face_at(0.5)], 0.58, &body, &mut runs);
    assert_ne!(back, body);
    people_with(&mut heads, &[face_at(0.5)], 0.66, &body, &mut runs);
    assert_eq!(runs, 3);
}

#[test]
fn nobody_at_all_reports_nobody() {
    let mut heads = Heads::new();
    let mut runs = 0;
    // No face yet is no face for long: the pose model looks, at its own pace
    assert!(people_with(&mut heads, &[], 0.0, &[], &mut runs).is_empty());
    assert_eq!(maybe_people(&mut heads, &[], 0.08, &[], &mut runs), None);
    assert_eq!(maybe_people(&mut heads, &[], GAP, &[], &mut runs), Some(Vec::new()));
    assert_eq!(runs, 2);
}

#[test]
fn restarting_checks_the_body_at_once_and_keeps_what_was_learned() {
    let mut heads = Heads::new();
    let mut runs = 0;
    for i in 0..6 {
        people_with(&mut heads, &[face_at(0.5)], i as f64, &[body_under(0.5, 2.4, 1.1)], &mut runs);
    }
    let shape = heads.shape();
    heads.restart();
    people_with(&mut heads, &[face_at(0.5)], 6.1, &[body_under(0.5, 2.4, 1.1)], &mut runs);
    assert_eq!(runs, 7);
    assert!((heads.shape().0 - shape.0).abs() < 0.1, "a fresh check nudges, it doesn't start over");
}

#[test]
fn a_failed_pose_run_is_an_error() {
    let mut heads = Heads::new();
    let failed = heads.people(&[face_at(0.5)], ASPECT, 0.0, GAP, &mut || Err(VisionError::new("RuntimeError", "broken")));
    assert!(failed.is_err());
}
