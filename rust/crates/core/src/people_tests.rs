//! test_people.py, by name.

use super::*;

// how often the pose model reports while something moves
const EVERY: f64 = 1.0 / 12.0;

fn person_at(x: f64, y: f64, width: f64, seen: f64) -> Person {
    let pts = [
        ("nose", x, y - 0.12), ("left_eye", x + 0.01, y - 0.14), ("right_eye", x - 0.01, y - 0.14),
        ("left_shoulder", x + width / 2.0, y), ("right_shoulder", x - width / 2.0, y),
        ("left_hip", x + width / 3.0, y + 0.35), ("right_hip", x - width / 3.0, y + 0.35),
    ];
    Person { points: pts.iter().map(|(n, px, py)| (n.to_string(), (*px, *py, seen))).collect() }
}

fn person(x: f64) -> Person {
    person_at(x, 0.4, 0.1, 0.99)
}

fn play(tracker: &mut PeopleTracker, frames: &[Vec<Person>], start: f64) -> Vec<(Vec<Track>, u32)> {
    frames.iter().enumerate().map(|(i, people)| {
        let tracks = tracker.update(people, start + i as f64 * EVERY);
        (tracks, tracker.count())
    }).collect()
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

#[test]
fn test_box_and_face_come_from_the_points_the_model_is_sure_of() {
    let p = person(0.5);
    let b = box_of(&p).unwrap();
    for (got, want) in [(b.0, 0.45), (b.1, 0.26), (b.2, 0.55), (b.3, 0.75)] {
        assert!(close(got, want, 1e-9), "{b:?}");
    }
    let f = face_of(&p).unwrap();
    assert!(close(f.0, 0.5, 1e-9) && close(f.1, 0.4 - 0.4 / 3.0, 1e-9));
    assert!(box_of(&person_at(0.5, 0.4, 0.1, 0.2)).is_none() && face_of(&person_at(0.5, 0.4, 0.1, 0.2)).is_none());
}

#[test]
fn test_someone_new_counts_after_two_updates_in_a_row() {
    let mut t = PeopleTracker::new();
    let counts: Vec<u32> = play(&mut t, &vec![vec![person(0.5)]; 3], 0.0).into_iter().map(|r| r.1).collect();
    assert_eq!(counts, [0, 1, 1]);
}

#[test]
fn test_one_update_with_a_ghost_doesnt_count() {
    let mut t = PeopleTracker::new();
    let mut frames = vec![vec![person(0.3)]; 4];
    frames.push(vec![person(0.3), person(0.8)]);
    frames.extend(vec![vec![person(0.3)]; 4]);
    assert_eq!(play(&mut t, &frames, 0.0).iter().map(|r| r.1).max(), Some(1));
}

#[test]
fn test_drop_outs_keep_the_same_person_and_the_count() {
    let mut t = PeopleTracker::new();
    let mut frames = vec![vec![person(0.5)]; 5];
    frames.extend(vec![vec![]; 10]);
    frames.extend(vec![vec![person(0.52)]; 3]);
    frames.extend(vec![vec![]; 5]);
    frames.extend(vec![vec![person(0.5)]; 2]);
    let results = play(&mut t, &frames, 0.0);
    assert!(results[2..].iter().all(|r| r.1 == 1));
    let ids: std::collections::BTreeSet<u64> = results.iter().flat_map(|r| r.0.iter().map(|t| t.id)).collect();
    assert_eq!(ids.into_iter().collect::<Vec<_>>(), [1]);
    assert!(close(results[14].0[0].missing_s, 10.0 * EVERY, 1e-9));
}

#[test]
fn test_someone_gone_for_good_is_let_go_slowly() {
    let mut t = PeopleTracker::new();
    play(&mut t, &vec![vec![person(0.5)]; 5], 0.0);
    let gone_at = 4.0 * EVERY;
    let mut now = gone_at;
    while now - gone_at < DROP_S - 0.1 {
        now += EVERY;
        t.update(&[], now);
        assert_eq!(t.count(), 1);
    }
    t.update(&[], gone_at + DROP_S + 0.01);
    assert_eq!(t.count(), 0);
    assert!(t.tracks(gone_at + DROP_S + 0.01).is_empty());
}

#[test]
fn test_a_second_person_walking_behind_keeps_both_ids() {
    let mut t = PeopleTracker::new();
    let frames: Vec<Vec<Person>> = (0..40).map(|i| {
        let walker = 0.1 + i as f64 * 0.02;
        let hidden = (walker - 0.5).abs() < 0.08;
        let mut f = vec![person_at(0.5, 0.4, 0.14, 0.99)];
        if !hidden {
            f.push(person_at(walker, 0.4, 0.08, 0.99));
        }
        f
    }).collect();
    let results = play(&mut t, &frames, 0.0);
    assert!(results[2..].iter().all(|r| r.1 == 2));
    let ids: std::collections::BTreeSet<u64> = results.iter().flat_map(|r| r.0.iter().map(|t| t.id)).collect();
    assert_eq!(ids.into_iter().collect::<Vec<_>>(), [1, 2]);
    let end = &results.last().unwrap().0;
    let x = |id| end.iter().find(|t| t.id == id).unwrap().centre().0;
    assert!(close(x(1), 0.5, 0.01) && x(2) > 0.8);
}

#[test]
fn test_boxes_are_smoothed_and_the_one_there_longest_is_main() {
    let mut t = PeopleTracker::new();
    play(&mut t, &vec![vec![person(0.3)]; 6], 0.0);
    t.update(&[person(0.3), person(0.7)], 6.0 * EVERY);
    let tracks = t.update(&[person(0.4), person(0.7)], 7.0 * EVERY);
    let you = main(&tracks).unwrap();
    assert!(you.id == 1 && 0.3 < you.centre().0 && you.centre().0 < 0.4);
    assert!(main(&[]).is_none());
}

#[test]
fn test_two_skeletons_on_one_person_count_once() {
    let mut t = PeopleTracker::new();
    let frames = vec![vec![person(0.5), person(0.505)]; 6];
    assert_eq!(play(&mut t, &frames, 0.0).iter().map(|r| r.1).max(), Some(1));
}

#[test]
fn test_tracking_is_kept_per_stream_and_forgets_what_stopped() {
    let mut tracking = PeopleTracking::new();
    for i in 0..3 {
        tracking.update("a", &[person(0.5)], i as f64 * EVERY);
    }
    assert_eq!(tracking.last("a", 0.5).1, 1);
    assert_eq!(tracking.last("b", 0.5), (vec![], 0));
    assert_eq!(tracking.last("a", 0.2 + DROP_S + 1.0), (vec![], 0));
    tracking.drop_stream("a");
    assert_eq!(tracking.update("a", &[person(0.5)], 10.0).1, 0);
    tracking.update("b", &[person(0.5)], 100.0);
    assert!(!tracking.tracking("a"));
}

#[test]
fn test_flipping_the_picture_keeps_everyone_and_their_ids() {
    let mut tracking = PeopleTracking::new();
    for i in 0..4 {
        tracking.update("a", &[person(0.3), person(0.8)], i as f64 * EVERY);
    }
    tracking.remap(&|x, y| (1.0 - x, y));
    let mut xs: Vec<f64> = tracking.last("a", 0.4).0.iter().map(|t| (t.centre().0 * 100.0).round() / 100.0).collect();
    xs.sort_by(f64::total_cmp);
    assert_eq!(xs, [0.2, 0.7]);
    let results: Vec<(Vec<Track>, u32)> = (0..3).map(|i| tracking.update("a", &[person(0.7), person(0.2)], (4 + i) as f64 * EVERY)).collect();
    assert!(results.iter().all(|r| r.1 == 2));
    let last = &results.last().unwrap().0;
    let x = |id| last.iter().find(|t| t.id == id).unwrap().centre().0;
    assert!(close(x(1), 0.7, 0.01) && close(x(2), 0.2, 0.01));
}

#[test]
fn a_track_goes_through_its_record_unchanged() {
    let t = Track { id: 3, bbox: (0.1, 0.2, 0.3, 0.4), face: Some((0.2, 0.25)), seen_s: 1.5, missing_s: 0.0 };
    assert_eq!(Track::from_value(&t.to_value()), Some(t.clone()));
    let faceless = Track { face: None, ..t };
    assert_eq!(Track::from_value(&faceless.to_value()), Some(faceless));
    assert_eq!(Track::from_value(&serde_json::json!({"id": 1})), None);
}
