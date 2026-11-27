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
    assert_eq!(exposure_text(0.0), "Video");
    assert_eq!(exposure_text(1.0 / 250.0), "1/250 s");
    assert_eq!(exposure_text(10.0), "10 s");
    // The phone's own focus number, not metres most phones don't get right
    assert_eq!(shot_focus_text(0.5), "0.50");
    assert_eq!(shot_focus_text(4.0), "4.00");
    assert_eq!(shot_focus_text(0.001), "∞");
    assert_eq!(ShotChoice { focus_diopters: Some(0.5), ..ShotChoice::default() }.summary(|_| "far".into()), "focus far");
}

#[test]
fn the_focus_scale_runs_from_infinity_to_the_nearest_distance() {
    assert_eq!(focus_position(10.0, 0.0), 0.0);
    assert_eq!(focus_position(10.0, 10.0), FOCUS_SCALE);
    assert_eq!(focus_position(10.0, 2.5), 250.0);
    assert_eq!(focus_position(0.0, 2.0), 0.0);
    assert_eq!(focus_position(10.0, 20.0), FOCUS_SCALE);
}

#[test]
fn discrete_camera_lists_offer_only_the_reported_numbers() {
    let c = caps(json!({"shutter_values_s": [30.0, 0.004, 10.0, 10.0, 0.0], "iso_values": [800, 100, 800, 0]}));
    assert_eq!(shot_shutter_stops(&c), [0.004, 10.0, 30.0]);
    assert_eq!(shot_iso_stops(&c), [100, 800]);
}

#[test]
fn shared_controls_suppress_overrides_without_erasing_choices() {
    let c: StillCaps = serde_json::from_value(json!({"shot": {"shutter_values_s": [0.004, 10.0], "iso_values": [100, 800]}})).unwrap();
    let mut choice = ShotChoice { shutter_s: Some(10.0), iso: Some(800), focus_diopters: Some(0.0) };
    let kept = choice.clone();
    let props = shot_control_props(Some(&c), &choice, true, true);
    assert_eq!(props["shot-iso-enabled"], false);
    assert_eq!(props["shot-shutter-enabled"], false);
    assert!(!pick_shot(&mut choice, &c, false, 0, true));
    let mut options = json!({});
    add_shot(&mut options, &c, &choice, true, true);
    assert_eq!(options, json!({}));
    assert_eq!(choice, kept);
    add_shot(&mut options, &c, &choice, false, false);
    // This phone takes no focus for a photo, so the kept one is left out and not shown
    assert_eq!(options, json!({"shot": {"iso": 800}}));
    assert_eq!(shot_control_props(Some(&c), &choice, false, false)["shot-summary"], "ISO 800");
    let near: StillCaps = serde_json::from_value(json!({"shot": {"iso_values": [100, 800], "focus": {"near_diopters": 10.0}}})).unwrap();
    assert_eq!(shot_control_props(Some(&near), &choice, false, false)["shot-summary"], "ISO 800 · focus ∞");
}

#[test]
fn the_shared_picker_can_unset_and_rejects_stale_indices() {
    let c: StillCaps = serde_json::from_value(json!({"shot": {"iso_values": [100, 800]}})).unwrap();
    let mut choice = ShotChoice::default();
    assert!(pick_shot(&mut choice, &c, false, 2, false));
    assert_eq!(choice.iso, Some(800));
    assert!(!pick_shot(&mut choice, &c, false, 3, false));
    assert!(pick_shot(&mut choice, &c, false, 0, false));
    assert_eq!(choice.iso, None);
    assert!(!pick_shot(&mut choice, &StillCaps::default(), true, 1, false));
}

#[test]
fn unavailable_shot_overrides_are_kept_for_validation() {
    let choice = ShotChoice { shutter_s: Some(10.0), iso: Some(800), ..ShotChoice::default() };
    let caps = StillCaps::default();
    let props = shot_control_props(Some(&caps), &choice, true, false);
    assert_eq!(props["shot-shutter-enabled"], true);
    assert_eq!(props["shot-iso-enabled"], true);
    let mut options = json!({});
    add_shot(&mut options, &caps, &choice, true, false);
    assert_eq!(options["shot"], json!({"shutter_s": 10.0, "iso": 800}));
    assert!(telescope_core::shot::apply_to_dslr(options.as_object_mut().unwrap(), &json!({})).is_err());
}

#[test]
fn the_shared_picker_clears_an_override_the_camera_cannot_set() {
    let mut choice = ShotChoice { shutter_s: Some(10.0), iso: Some(800), focus_diopters: None };
    assert!(pick_shot(&mut choice, &StillCaps::default(), true, 0, false));
    assert!(pick_shot(&mut choice, &StillCaps::default(), false, 0, false));
    assert_eq!(choice, ShotChoice::default());
    assert!(!pick_shot(&mut choice, &StillCaps::default(), false, 0, false));
}

#[test]
fn an_override_the_camera_cannot_set_keeps_its_control_enabled_to_clear_it() {
    let choice = ShotChoice { shutter_s: Some(10.0), iso: Some(800), focus_diopters: None };
    for caps in [Some(StillCaps::default()), None] {
        let props = shot_control_props(caps.as_ref(), &choice, true, false);
        assert_eq!(props["shot-shutter-enabled"], true);
        assert_eq!(props["shot-iso-enabled"], true);
    }
    assert_eq!(shot_control_props(None, &ShotChoice::default(), true, false)["shot-iso-enabled"], false);
    assert_eq!(shot_control_props(None, &choice, true, true)["shot-iso-enabled"], false);
}
