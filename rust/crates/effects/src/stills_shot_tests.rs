use super::*;

fn caps(value: Value) -> ShotCaps {
    serde_json::from_value(value).unwrap()
}

#[test]
fn shutter_stops_include_the_devices_ends_and_night_times() {
    let c = caps(json!({"shutter": {"min_ns": 62_500, "max_ns": 23_000_000_000i64}}));
    let stops = shot_shutter_stops(&c);
    assert_eq!(stops.first(), Some(&0.0000625));
    assert_eq!(stops.last(), Some(&23.0));
    for speed in [1.0 / 4000.0, 1.0, 8.0, 10.0, 15.0, 20.0] {
        assert!(stops.contains(&speed), "{speed}");
    }
    assert!(stops.windows(2).all(|s| s[0] < s[1]));
    assert!(stops.iter().all(|s| *s >= 0.0000625 && *s <= 23.0));
}

#[test]
fn shutter_ends_close_to_a_stop_are_not_repeated() {
    let c = caps(json!({"shutter": {"min_ns": 999_500_000, "max_ns": 10_005_000_000i64}}));
    assert_eq!(shot_shutter_stops(&c), vec![1.0, 2.0, 4.0, 8.0, 10.0]);
    let c = caps(json!({"shutter": {"min_ns": 2_000_000_000i64, "max_ns": 2_000_000_000i64}}));
    assert_eq!(shot_shutter_stops(&c), vec![2.0]);
}

#[test]
fn photos_shutter_list_stays_in_whole_stops() {
    let c = StillCaps { exposure_min_ns: 250_000, exposure_max_ns: 120_000_000_000, ..StillCaps::default() };
    let expected: Vec<f64> = FAST_N.iter().map(|n| 1.0 / f64::from(*n)).chain(LONG_S).collect();
    assert_eq!(shutter_speeds(&c), expected);
    let c = StillCaps { exposure_min_ns: 0, ..c };
    assert_eq!(shutter_speeds(&c), LONG_S.to_vec());
}

#[test]
fn iso_stops_are_thirds_clipped_with_the_device_max_added() {
    let c = caps(json!({"iso": {"min": 70, "max": 900}}));
    assert_eq!(shot_iso_stops(&c), vec![80, 100, 125, 160, 200, 250, 320, 400, 500, 640, 800, 900]);
    let c = caps(json!({"iso": {"min": 800, "max": 800}}));
    assert_eq!(shot_iso_stops(&c), vec![800]);
    let c = caps(json!({"iso": {"min": 30000, "max": 32000}}));
    assert_eq!(shot_iso_stops(&c), vec![32000]);
}

#[test]
fn absent_or_invalid_ranges_offer_no_stops() {
    for value in [json!({}), json!({"shutter": {"min_ns": 0, "max_ns": 10}, "iso": {"min": 400, "max": 100}})] {
        let c = caps(value);
        assert!(shot_shutter_stops(&c).is_empty());
        assert!(shot_iso_stops(&c).is_empty());
    }
    assert!(serde_json::from_value::<StillCaps>(json!({})).unwrap().shot.is_none());
}

#[test]
fn choices_round_trip_only_their_own_keys() {
    for value in [json!({}), json!({"iso": 1600}), json!({"shutter_s": 10.0, "iso": 800, "focus_diopters": 0.0})] {
        let choice: ShotChoice = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(choice.to_options_value(), value);
        assert_eq!(serde_json::to_value(choice).unwrap(), value);
    }
    let choice: ShotChoice = serde_json::from_value(json!({"iso": 800, "other": true})).unwrap();
    assert_eq!(choice.to_options_value(), json!({"iso": 800}));
}

#[test]
fn summaries_show_own_values_in_order() {
    let choice = ShotChoice { shutter_s: Some(10.0), iso: Some(800), focus_diopters: Some(0.0) };
    assert_eq!(choice.summary(shot_focus_text), "10 s · ISO 800 · focus ∞");
    assert_eq!(ShotChoice { iso: Some(1600), ..ShotChoice::default() }.summary(shot_focus_text), "ISO 1600");
    assert_eq!(ShotChoice::default().summary(shot_focus_text), "Like the camera");
    assert_eq!(shot_exposure_text(1.0 / 250.0), "1/250 s");
    assert_eq!(shot_focus_text(0.5), "2 m");
    assert_eq!(shot_focus_text(4.0), "25 cm");
    assert_eq!(choice.summary(|_| "Infinity".into()), "10 s · ISO 800 · focus ∞");
    assert_eq!(ShotChoice { focus_diopters: Some(0.5), ..ShotChoice::default() }.summary(|_| "far".into()), "focus far");
}

#[test]
fn the_focus_scale_runs_from_infinity_to_the_nearest_distance() {
    assert_eq!(focus_position(10.0, 0.0), 0.0);
    assert_eq!(focus_position(10.0, 10.0), FOCUS_SCALE);
    for d in [0.0, 0.5, 2.5, 10.0] {
        assert!((focus_diopters(10.0, focus_position(10.0, d)) - d).abs() < 1e-10);
    }
    assert_eq!(focus_position(0.0, 2.0), 0.0);
    assert_eq!(focus_position(10.0, 20.0), FOCUS_SCALE);
    assert_eq!(focus_diopters(10.0, -1.0), 0.0);
    assert_eq!(focus_diopters(10.0, 2000.0), 10.0);
}
