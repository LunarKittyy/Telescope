use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use telescope_core::host::Pick;
use telescope_core::testing::{scratch_dir, FakeHost};

fn caps() -> Value {
    json!({"burst_max": 8, "extensions": ["night"]})
}

fn host() -> FakeHost {
    let mut h = FakeHost::new();
    h.streaming = true;
    h.focused = Some("phone".into());
    h.plugins = vec!["preview".into()];
    h.still_caps = Some(caps());
    h
}

/// Every property the card has been given, the latest of each.
fn card(h: &FakeHost) -> Value {
    let mut all = serde_json::Map::new();
    for call in &h.calls {
        if let Some(json) = call.strip_prefix("set_ui_props card ") {
            if let Ok(Value::Object(props)) = serde_json::from_str(json) {
                all.extend(props);
            }
        }
    }
    Value::Object(all)
}

fn seen(p: &mut PhototrapPlugin, h: &mut FakeHost, source: &str, zones: &[u32]) {
    p.on_event(h, &Event::MotionSeen { source_id: source.into(), owner: NAME.into(), zones: zones.into() });
}

fn finished(p: &mut PhototrapPlugin, h: &mut FakeHost, request: u64, ok: bool, data: Value) {
    p.on_event(h, &Event::RequestDone { request: RequestId(request), ok, message: String::new(), data });
}

/// A plugin on a clock the test moves, in seconds.
fn plugin() -> (PhototrapPlugin, Arc<AtomicU64>) {
    let now = Arc::new(AtomicU64::new(100));
    let seen = now.clone();
    let mut p = PhototrapPlugin::with_clock(move || seen.load(Ordering::SeqCst) as f64);
    p.folder = "/photos/Phototrap".into();
    // Whatever the machine running the tests has free
    p.free = Box::new(|_| Some(u64::MAX));
    (p, now)
}

fn click(p: &mut PhototrapPlugin, h: &mut FakeHost, callback: &str, arg: Value) {
    p.on_ui(h, &UiEvent { ui: "card".into(), callback: callback.into(), args: vec![arg] });
}

/// Something moves in the first zone again: it was still, then it isn't.
fn moved(p: &mut PhototrapPlugin, h: &mut FakeHost) {
    seen(p, h, "phone", &[]);
    seen(p, h, "phone", &[0]);
}

fn pick(a: [f32; 2], b: [f32; 2]) -> Event {
    let points = vec![a, b];
    Event::PreviewPicked { owner: NAME.into(), pick: Some(Pick { kind: PickKind::Rect, shown: points.clone(), full: points.clone(), in_picture: points, picture: None }) }
}

fn watches(h: &FakeHost) -> Vec<&String> {
    h.calls.iter().filter(|c| c.starts_with("watch_motion")).collect()
}

fn armed() -> (PhototrapPlugin, FakeHost, Arc<AtomicU64>) {
    let (mut p, now) = plugin();
    let mut h = host();
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(true));
    (p, h, now)
}

#[test]
fn arming_watches_the_zones() {
    let (mut p, mut h) = (plugin().0, host());
    p.on_event(&mut h, &pick([0.1, 0.1], [0.5, 0.6]));
    p.on_event(&mut h, &pick([0.6, 0.2], [0.9, 0.4]));
    assert!(watches(&h).is_empty(), "nothing is watched until it's armed");
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(true));
    assert_eq!(watches(&h), ["watch_motion phototrap 2 5"]);
    assert!(h.layers.contains_key(NAME));
}

#[test]
fn with_no_zones_the_whole_picture_is_watched() {
    let (_, h, _) = armed();
    assert_eq!(watches(&h), ["watch_motion phototrap 1 5"]);
}

#[test]
fn motion_takes_one_burst_with_the_right_options() {
    let (mut p, mut h, _) = armed();
    click(&mut p, &mut h, "count-moved", json!(4.0));
    moved(&mut p, &mut h);
    assert_eq!(h.stills_taken.len(), 1);
    let (options, folder) = &h.stills_taken[0];
    assert_eq!(folder, "/photos/Phototrap");
    assert_eq!(options["count"], 4);
    assert_eq!(options["format"], "jpeg");
    assert!(options.get("extension").is_none());
}

#[test]
fn night_asks_for_the_makers_mode_only_when_the_lens_has_it() {
    let (mut p, mut h, _) = armed();
    click(&mut p, &mut h, "night-toggled", json!(true));
    moved(&mut p, &mut h);
    assert_eq!(h.stills_taken[0].0["extension"], "night");

    let (mut p, mut h, _) = armed();
    h.still_caps = Some(json!({"burst_max": 8}));
    p.on_event(&mut h, &Event::StillCapsChanged { source_id: "phone".into() });
    click(&mut p, &mut h, "night-toggled", json!(true));
    moved(&mut p, &mut h);
    assert!(h.stills_taken[0].0.get("extension").is_none());
}

#[test]
fn a_lens_without_bursts_takes_one_photo() {
    let (mut p, mut h, _) = armed();
    h.still_caps = Some(json!({"burst_max": 1}));
    p.on_event(&mut h, &Event::StillCapsChanged { source_id: "phone".into() });
    moved(&mut p, &mut h);
    assert!(h.stills_taken[0].0.get("count").is_none());
}

#[test]
fn a_second_motion_during_the_pause_or_while_busy_takes_nothing() {
    let (mut p, mut h, now) = armed();
    moved(&mut p, &mut h);
    // The burst ends, then the pause runs from there
    p.on_event(&mut h, &Event::RequestDone { request: RequestId(1), ok: true, message: String::new(), data: json!({"files": ["/a.jpg", "/b.jpg", "/c.jpg"]}) });
    now.store(105, Ordering::SeqCst);
    moved(&mut p, &mut h);
    assert_eq!(h.stills_taken.len(), 1, "inside the pause");
    now.store(120, Ordering::SeqCst);
    h.still_busy = true;
    moved(&mut p, &mut h);
    assert_eq!(h.stills_taken.len(), 1, "the phone is busy");
    h.still_busy = false;
    moved(&mut p, &mut h);
    assert_eq!(h.stills_taken.len(), 2);
    assert_eq!((p.triggers, p.photos), (2, 3));
}

#[test]
fn motion_for_another_owner_or_when_disarmed_takes_nothing() {
    let (mut p, mut h, _) = armed();
    p.on_event(&mut h, &Event::MotionSeen { source_id: "phone".into(), owner: "other".into(), zones: vec![0] });
    seen(&mut p, &mut h, "phone", &[]);
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(false));
    moved(&mut p, &mut h);
    assert!(h.stills_taken.is_empty());
}

#[test]
fn disarming_and_a_stream_stop_stop_the_watch() {
    let (mut p, mut h, _) = armed();
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(false));
    assert_eq!(watches(&h).last().unwrap().as_str(), "watch_motion phototrap 0 5");

    let (mut p, mut h, _) = armed();
    h.streaming = false;
    p.on_event(&mut h, &Event::StreamStopped);
    assert_eq!(watches(&h).last().unwrap().as_str(), "watch_motion phototrap 0 5");
    h.streaming = true;
    p.on_event(&mut h, &Event::StreamStarted { url: String::new() });
    assert_eq!(watches(&h).last().unwrap().as_str(), "watch_motion phototrap 1 5");
}

#[test]
fn without_a_phone_that_takes_photos_it_waits_and_says_so() {
    let (mut p, now) = plugin();
    let _ = now;
    let mut h = FakeHost::new();
    h.streaming = true;
    h.focused = Some("phone".into());
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(true));
    assert!(watches(&h).is_empty());
    let status = h.calls.iter().rev().find(|c| c.starts_with("set_ui_props card")).unwrap();
    assert!(status.contains("Waiting for a phone that can take photos"), "{status}");
    h.still_caps = Some(caps());
    p.on_event(&mut h, &Event::StillCapsChanged { source_id: "phone".into() });
    assert_eq!(watches(&h), ["watch_motion phototrap 1 5"]);
}

#[test]
fn config_round_trips_and_armed_is_not_saved() {
    let (mut p, mut h, _) = armed();
    p.on_event(&mut h, &pick([0.1, 0.1], [0.5, 0.6]));
    click(&mut p, &mut h, "sensitivity-moved", json!(8.0));
    click(&mut p, &mut h, "count-moved", json!(5.0));
    click(&mut p, &mut h, "pause-moved", json!(30.0));
    click(&mut p, &mut h, "night-toggled", json!(true));
    let saved = p.get_config();
    assert!(saved.get("armed").is_none() && saved.get("on").is_none());
    let mut q = PhototrapPlugin::new();
    q.set_config(&saved).unwrap();
    assert_eq!(q.get_config(), saved);
    assert!(!q.armed);
    assert_eq!((q.sensitivity, q.count, q.pause_s, q.night, q.zones.len()), (8, 5, 30, true, 1));
}

#[test]
fn bad_settings_fall_back() {
    let mut p = PhototrapPlugin::new();
    p.set_config(&json!({"zones": [[0.5, 0.5, 0.4, 0.9], [0.1, 0.1, 0.3, 0.3], "x", [0.0, 0.0, 2.0, 1.0]], "sensitivity": 99, "count": "many", "pause": -4})).unwrap();
    assert_eq!(p.zones, [[0.1, 0.1, 0.3, 0.3]]);
    assert_eq!((p.sensitivity, p.count, p.pause_s), (10, DEFAULT_COUNT, 1));
}

#[test]
fn zones_come_from_a_pick_up_to_four() {
    let (mut p, mut h) = (plugin().0, host());
    click(&mut p, &mut h, "draw-clicked", json!(null));
    assert!(h.calls.contains(&"pick_on_preview phototrap Rect".to_string()));
    p.on_event(&mut h, &pick([0.5, 0.6], [0.1, 0.2]));
    assert_eq!(p.zones, [[0.1, 0.2, 0.5, 0.6]], "corners in any order");
    p.on_event(&mut h, &pick([0.3, 0.3], [0.3, 0.9]));
    assert_eq!(p.zones.len(), 1, "a line is no zone");
    for i in 0..5 {
        p.on_event(&mut h, &pick([0.1, 0.1], [0.2 + i as f32 * 0.01, 0.5]));
    }
    assert_eq!(p.zones.len(), MAX_ZONES);
    h.calls.clear();
    click(&mut p, &mut h, "draw-clicked", json!(null));
    assert!(h.calls.is_empty(), "no fifth zone");
    click(&mut p, &mut h, "clear-clicked", json!(null));
    assert!(p.zones.is_empty());
    let Some(layer) = h.layers.get(NAME) else { panic!("the layer stays") };
    assert!(layer.shapes.is_empty());
}

#[test]
fn zones_show_on_the_preview_while_the_window_is_up_or_it_is_armed() {
    let (mut p, mut h) = (plugin().0, host());
    p.on_event(&mut h, &pick([0.1, 0.1], [0.5, 0.6]));
    assert_eq!(h.layers[NAME].shapes.len(), 2);
    p.on_event(&mut h, &Event::WindowShown { shown: false });
    assert!(!h.layers.contains_key(NAME));
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(true));
    assert!(h.layers.contains_key(NAME));
}

#[test]
fn the_folder_is_phototrap_inside_the_photos_folder() {
    let p = PhototrapPlugin::with_folder("/x");
    assert_eq!(p.folder, "/x");
    assert!(PhototrapPlugin::new().folder.ends_with("Phototrap"));
}

#[test]
fn only_motion_on_the_armed_stream_takes_photos_and_every_call_names_it() {
    let (mut p, mut h, _) = armed();
    h.still_only_for = Some("phone".into());
    seen(&mut p, &mut h, "other", &[]);
    seen(&mut p, &mut h, "other", &[0]);
    assert!(h.stills_taken.is_empty(), "another phone's movement");
    // The panels move on to the other phone, and the trap stays where it was armed
    h.focused = Some("other".into());
    h.live = vec!["phone".into(), "other".into()];
    p.on_event(&mut h, &Event::SourceSelected { source_id: "other".into() });
    moved(&mut p, &mut h);
    assert_eq!(h.stills_taken.len(), 1);
    assert_eq!(h.still_sources, [Some("phone".to_string())]);
    assert_eq!(watches(&h), ["watch_motion phototrap 1 5"], "still watching");
}

#[test]
fn arming_before_a_stream_picks_up_the_one_that_comes() {
    let (mut p, now) = plugin();
    let _ = now;
    let mut h = FakeHost::new();
    h.plugins = vec!["preview".into()];
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(true));
    assert!(watches(&h).is_empty());
    h.streaming = true;
    h.focused = Some("phone".into());
    h.still_caps = Some(caps());
    p.on_event(&mut h, &Event::StreamConnected { source_id: "phone".into() });
    assert_eq!(watches(&h), ["watch_motion phototrap 1 5"]);
    assert_eq!(p.source.as_deref(), Some("phone"));
    // And the caps arriving after the stream did are heard in the phone's state
    let (mut p, mut h, _) = armed();
    h.still_caps = None;
    p.on_event(&mut h, &Event::StillCapsChanged { source_id: "phone".into() });
    assert_eq!(watches(&h).last().unwrap().as_str(), "watch_motion phototrap 0 5");
    h.still_caps = Some(caps());
    p.on_event(&mut h, &Event::PhoneStateUpdated { state: json!({}) });
    assert_eq!(watches(&h).last().unwrap().as_str(), "watch_motion phototrap 1 5");
    let before = h.calls.len();
    p.on_event(&mut h, &Event::PhoneStateUpdated { state: json!({}) });
    p.on_event(&mut h, &Event::StreamState { source_id: "phone".into(), state: json!({}) });
    assert_eq!(h.calls.len(), before, "nothing changed, so nothing is said again");
}

#[test]
fn nothing_is_taken_while_the_picture_is_hidden_and_the_host_is_asked_to_check_again() {
    let (mut p, mut h, _) = armed();
    h.picture_hidden = true;
    moved(&mut p, &mut h);
    assert!(h.stills_taken.is_empty());
    assert_eq!(card(&h)["status"], "Paused while the picture is hidden");
    assert_eq!(card(&h)["status-kind"], "status_warn");
    assert_eq!(p.triggers, 0);
    h.picture_hidden = false;
    moved(&mut p, &mut h);
    assert_eq!(h.stills_taken.len(), 1);
    assert_eq!(h.stills_taken[0].0["skip_if_hidden"], true);
    // The host found it hidden by the time it went to the phone: that's no trigger and no error
    finished(&mut p, &mut h, 1, false, json!({"ok": false, "files": [], "code": "hidden", "error": "hidden"}));
    assert_eq!((p.triggers, p.photos), (0, 0));
    assert!(p.problem.is_none());
    moved(&mut p, &mut h);
    assert_eq!(h.stills_taken.len(), 2, "no pause was started");
}

#[test]
fn a_zone_has_to_be_entered_to_set_it_off() {
    let (mut p, mut h, now) = armed();
    seen(&mut p, &mut h, "phone", &[0]);
    assert_eq!(h.stills_taken.len(), 1);
    finished(&mut p, &mut h, 1, true, json!({"files": ["/a.jpg"]}));
    now.store(200, Ordering::SeqCst);
    seen(&mut p, &mut h, "phone", &[0, 1]);
    assert_eq!(h.stills_taken.len(), 2, "a second zone starts moving");
    finished(&mut p, &mut h, 2, true, json!({"files": ["/a.jpg"]}));
    now.store(400, Ordering::SeqCst);
    seen(&mut p, &mut h, "phone", &[0]);
    assert_eq!(h.stills_taken.len(), 2, "one of two stops moving: that's nothing new");
    seen(&mut p, &mut h, "phone", &[0]);
    seen(&mut p, &mut h, "phone", &[]);
    assert_eq!(h.stills_taken.len(), 2, "and all stopping isn't either");
}

#[test]
fn photos_saved_before_a_burst_failed_still_count() {
    let (mut p, mut h, _) = armed();
    moved(&mut p, &mut h);
    finished(&mut p, &mut h, 1, false, json!({"ok": false, "files": ["/a.jpg", "/b.jpg"], "error": "The phone ran out of room"}));
    assert_eq!((p.triggers, p.photos), (1, 2));
    assert_eq!(card(&h)["status"], "The phone ran out of room");
    assert_eq!(card(&h)["status-kind"], "status_err");
}

#[test]
fn a_burst_cut_short_by_hiding_still_counts_what_it_saved_and_what_it_filled() {
    let run = |budget: u64| {
        let dir = scratch_dir(&format!("phototrap-cut-short-{budget}"));
        let (mut p, _) = plugin();
        p.folder = dir.display().to_string();
        p.budget = budget;
        let mut h = host();
        click(&mut p, &mut h, "switch-toggled-by-hand", json!(true));
        moved(&mut p, &mut h);
        let files: Vec<String> = ["a.jpg", "b.jpg"].iter().map(|n| {
            std::fs::write(dir.join(n), vec![0u8; 600]).unwrap();
            dir.join(n).display().to_string()
        }).collect();
        // Two shots were saved before the picture went hidden for the third
        finished(&mut p, &mut h, 1, false, json!({"ok": false, "files": files, "code": "hidden", "error": ""}));
        p
    };
    let roomy = run(10_000);
    assert_eq!((roomy.triggers, roomy.photos, roomy.used), (1, 2, 1200), "the files and their bytes count");
    assert!(roomy.problem.is_none() && roomy.armed, "hiding is no error and the folder has room");
    let full = run(1000);
    assert_eq!((full.triggers, full.photos, full.used), (1, 2, 1200));
    assert!(!full.armed, "the saved files filled the folder");
}

#[test]
fn a_zone_is_kept_where_the_detector_looks_whatever_the_zoom_and_flip() {
    let (mut p, mut h, _) = armed();
    // Zoomed 2x on the middle, a drag across the left of the preview is over the part of the picture that starts a quarter in
    h.view.zoom = 2.0;
    let on_preview = [[0.0, 0.2], [0.2, 0.6]];
    let in_picture = vec![[0.25, 0.35], [0.35, 0.55]];
    let drag = Pick { kind: PickKind::Rect, shown: on_preview.to_vec(), full: in_picture.clone(), in_picture, picture: None };
    p.on_event(&mut h, &Event::PreviewPicked { owner: NAME.into(), pick: Some(drag) });
    let close = |a: [f32; 4], b: [f32; 4]| a.iter().zip(b).all(|(x, y)| (x - y).abs() < 1e-5);
    assert!(close(p.zones[0], [0.25, 0.35, 0.35, 0.55]), "{:?}", p.zones[0]);
    assert_eq!(watches(&h).last().unwrap().as_str(), "watch_motion phototrap 1 5");
    // And drawn back where the drag was
    let Shape::Rect { x0, y0, x1, y1, .. } = h.layers[NAME].shapes[0].clone() else { panic!("a rectangle") };
    assert!(close([x0, y0, x1, y1], [0.0, 0.2, 0.2, 0.6]), "{x0} {y0} {x1} {y1}");

    // A flipped view turns the detector's picture, and flipping it later takes the zone along
    let (mut p, mut h, _) = armed();
    h.view.flip_h = true;
    let view = h.view.clone();
    p.on_event(&mut h, &Event::ViewChanged { view });
    let points = vec![[0.1, 0.1], [0.3, 0.5]];
    let drag = Pick { kind: PickKind::Rect, shown: points.clone(), full: vec![[0.9, 0.1], [0.7, 0.5]], in_picture: points, picture: None };
    p.on_event(&mut h, &Event::PreviewPicked { owner: NAME.into(), pick: Some(drag) });
    assert!(close(p.zones[0], [0.1, 0.1, 0.3, 0.5]), "{:?}", p.zones[0]);
    assert_eq!(p.get_config()["turn"]["flip_h"], true);
    h.view.flip_h = false;
    let view = h.view.clone();
    p.on_event(&mut h, &Event::ViewChanged { view });
    assert!(close(p.zones[0], [0.7, 0.1, 0.9, 0.5]), "{:?}", p.zones[0]);
    let Shape::Rect { x0, x1, .. } = h.layers[NAME].shapes[0].clone() else { panic!("a rectangle") };
    assert!(close([x0, 0.0, x1, 0.0], [0.7, 0.0, 0.9, 0.0]));
    assert!(watches(&h).last().unwrap().ends_with(" 1 5"), "watched again, in the new places");
}

#[test]
fn zones_saved_before_the_view_was_turned_follow_it() {
    let mut p = PhototrapPlugin::new();
    p.set_config(&json!({"zones": [[0.1, 0.1, 0.3, 0.5]], "turn": {"flip_h": true, "flip_v": false, "rotation": null}})).unwrap();
    let mut h = host();
    p.setup(&mut h);
    assert!(p.zones[0].iter().zip([0.7, 0.1, 0.9, 0.5]).all(|(x, y)| (x - y).abs() < 1e-5), "{:?}", p.zones[0]);
}

#[test]
fn a_full_folder_disarms_it_and_says_why() {
    let dir = scratch_dir("phototrap-full");
    std::fs::write(dir.join("a.jpg"), vec![0u8; 600]).unwrap();
    std::fs::create_dir(dir.join("older")).unwrap();
    std::fs::write(dir.join("older").join("b.jpg"), vec![0u8; 600]).unwrap();
    let (mut p, now) = plugin();
    let _ = now;
    p.folder = dir.display().to_string();
    p.budget = 1000;
    let mut h = host();
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(true));
    assert!(!p.armed, "what's there already is over the limit");
    assert!(watches(&h).last().unwrap().ends_with(" 0 5"));
    assert_eq!(card(&h)["armed"], false);
    assert_eq!(card(&h)["status-kind"], "status_err");
    assert!(card(&h)["status"].as_str().unwrap().starts_with("Off since the "), "{}", card(&h)["status"]);

    // A burst that fills it is the last one
    std::fs::remove_dir_all(dir.join("older")).unwrap();
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(true));
    assert!(p.armed);
    moved(&mut p, &mut h);
    let big = dir.join("c.jpg");
    std::fs::write(&big, vec![0u8; 600]).unwrap();
    finished(&mut p, &mut h, 1, true, json!({"files": [big.display().to_string()]}));
    assert!(!p.armed);
    assert_eq!(p.photos, 1);
    assert_eq!(card(&h)["status-kind"], "status_err");
    // Switching it on again clears the line when there's room
    p.budget = 5000;
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(true));
    assert!(p.armed && p.problem.is_none());
}

#[test]
fn too_little_room_on_the_drive_disarms_it_before_a_burst() {
    let (mut p, mut h, _) = armed();
    p.min_free = 1000;
    p.free = Box::new(|_| Some(999));
    moved(&mut p, &mut h);
    assert!(h.stills_taken.is_empty());
    assert!(!p.armed);
    assert_eq!(card(&h)["status-kind"], "status_err");
    assert!(card(&h)["status"].as_str().unwrap().starts_with("Off since there's less than"), "{}", card(&h)["status"]);
    assert!(watches(&h).last().unwrap().ends_with(" 0 5"));
    // A drive that can't say how much is free is no reason to stop
    let (mut p, mut h, _) = armed();
    p.min_free = 1000;
    p.free = Box::new(|_| None);
    moved(&mut p, &mut h);
    assert_eq!(h.stills_taken.len(), 1);
}

#[test]
fn limits_are_told_in_gigabytes() {
    assert_eq!(gigabytes(STORAGE_BUDGET), "2");
    assert_eq!(gigabytes(MIN_FREE), "1");
    assert_eq!(gigabytes(1 << 29), "0.5");
}

#[test]
fn draw_zone_waits_for_the_preview_and_says_so() {
    let (mut p, mut h, _) = armed();
    h.plugins.clear();
    p.on_event(&mut h, &Event::PhonesChanged { count: 1 });
    p.sync(&mut h);
    assert_eq!(card(&h)["draw-enabled"], false);
    assert_eq!(card(&h)["draw-tip"], DRAW_OFF_TIP);
    h.calls.clear();
    click(&mut p, &mut h, "draw-clicked", json!(null));
    assert!(!h.calls.iter().any(|c| c.starts_with("pick_on_preview")));
    h.plugins = vec!["preview".into()];
    p.sync(&mut h);
    assert_eq!(card(&h)["draw-enabled"], true);
    assert_eq!(card(&h)["draw-tip"], DRAW_TIP);
}

#[test]
fn the_card_names_the_folder_and_opens_it() {
    let dir = scratch_dir("phototrap-folder").join("Phototrap");
    let (mut p, mut h, _) = armed();
    p.folder = dir.display().to_string();
    p.sync(&mut h);
    assert_eq!(card(&h)["folder-tip"], dir.display().to_string());
    click(&mut p, &mut h, "folder-clicked", json!(null));
    assert!(dir.is_dir(), "made so there's something to open");
    assert_eq!(h.calls.last().unwrap(), &format!("open_url {}", file_url(&dir)));
    // The home folder is only a ~ on the card
    if let Some(home) = std::env::home_dir() {
        let p = PhototrapPlugin::with_folder(&home.join("Pictures").join("Phototrap").display().to_string());
        assert_eq!(p.shown_folder(), Path::new("~").join("Pictures").join("Phototrap").display().to_string());
    }
}

#[test]
fn zones_are_drawn_and_seen_only_on_the_stream_it_is_armed_on() {
    let (mut p, mut h, _) = armed();
    h.can_pick = true;
    h.live = vec!["phone".into(), "other".into()];
    p.on_event(&mut h, &pick([0.1, 0.1], [0.5, 0.6]));
    assert!(h.layers.contains_key(NAME));
    assert_eq!(card(&h)["draw-enabled"], true);
    // The panels show another phone: nothing is drawn over its picture and no zone can be added there
    h.focused = Some("other".into());
    p.on_event(&mut h, &Event::SourceSelected { source_id: "other".into() });
    assert!(!h.layers.contains_key(NAME));
    assert_eq!(card(&h)["draw-enabled"], false);
    assert_eq!(card(&h)["draw-tip"], DRAW_ELSEWHERE_TIP);
    h.calls.clear();
    click(&mut p, &mut h, "draw-clicked", json!(null));
    assert!(!h.calls.iter().any(|c| c.starts_with("pick_on_preview")));
    p.on_event(&mut h, &pick([0.6, 0.2], [0.9, 0.4]));
    assert_eq!(p.zones.len(), 1, "a pick that was under way is no zone of the armed stream");
    assert_eq!(p.watch.as_ref().map(|w| w.0.len()), Some(1), "the watch is as it was");
    // A pick under way when the panels move on is cancelled
    h.focused = Some("phone".into());
    p.on_event(&mut h, &Event::SourceSelected { source_id: "phone".into() });
    assert!(h.layers.contains_key(NAME));
    assert_eq!(card(&h)["draw-enabled"], true);
    assert_eq!(card(&h)["draw-tip"], DRAW_TIP);
    click(&mut p, &mut h, "draw-clicked", json!(null));
    assert!(p.drawing);
    h.focused = Some("other".into());
    p.on_event(&mut h, &Event::SourceSelected { source_id: "other".into() });
    assert!(!p.drawing);
    assert!(h.calls.contains(&"cancel_pick phototrap".to_string()));
    // Not armed, zones are drawn on whatever the panels show
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(false));
    assert!(h.layers.contains_key(NAME));
    assert_eq!(card(&h)["draw-enabled"], true);
}

#[test]
fn a_burst_under_way_when_the_stream_stops_still_counts_when_it_ends() {
    let dir = scratch_dir("phototrap-interrupted");
    let (mut p, now) = plugin();
    p.folder = dir.display().to_string();
    p.budget = 1000;
    let mut h = host();
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(true));
    moved(&mut p, &mut h);
    let (a, b) = (dir.join("a.jpg"), dir.join("b.jpg"));
    std::fs::write(&a, vec![0u8; 400]).unwrap();
    std::fs::write(&b, vec![0u8; 400]).unwrap();
    h.streaming = false;
    h.still_caps = None;
    p.on_event(&mut h, &Event::StreamStopped);
    assert_eq!(p.taking.len(), 1, "still tracked");
    now.store(130, Ordering::SeqCst);
    p.on_event(&mut h, &Event::StreamStarted { url: String::new() });
    assert_eq!(p.taking.len(), 1, "a restart doesn't forget it either");
    let files = json!([a.display().to_string(), b.display().to_string()]);
    finished(&mut p, &mut h, 1, false, json!({"ok": false, "files": files, "code": "interrupted", "error": "The photo was interrupted by a change to the camera."}));
    assert_eq!((p.photos, p.used), (2, 800));
    assert!(p.taking.is_empty());
    assert!(p.problem.is_none(), "the stream stopping isn't a fault of the phone's");
    // And the next burst that fills the folder disarms it
    p.armed = true;
    let c = dir.join("c.jpg");
    std::fs::write(&c, vec![0u8; 400]).unwrap();
    h.streaming = true;
    h.still_caps = Some(caps());
    p.taking.push((RequestId(9), None));
    finished(&mut p, &mut h, 9, true, json!({"files": [c.display().to_string()]}));
    assert!(!p.armed, "the folder total was kept through the stop");
}

#[test]
fn a_burst_nobody_hears_the_end_of_is_given_up_on_a_minute_after_its_stream_stopped() {
    let (mut p, mut h, now) = armed();
    moved(&mut p, &mut h);
    h.streaming = false;
    p.on_event(&mut h, &Event::StreamStopped);
    now.store(150, Ordering::SeqCst);
    p.on_event(&mut h, &Event::PhonesChanged { count: 1 });
    assert_eq!(p.taking.len(), 1, "50 s on");
    now.store(161, Ordering::SeqCst);
    p.on_event(&mut h, &Event::PhonesChanged { count: 1 });
    assert!(p.taking.is_empty());
    // One that arrives after that is nobody's
    finished(&mut p, &mut h, 1, true, json!({"files": ["/a.jpg"]}));
    assert_eq!(p.photos, 0);
}

#[test]
fn a_burst_in_the_middle_of_a_long_exposure_is_not_given_up_on_while_its_stream_runs() {
    let (mut p, mut h, now) = armed();
    moved(&mut p, &mut h);
    now.store(1000, Ordering::SeqCst);
    p.on_event(&mut h, &Event::PhonesChanged { count: 1 });
    assert_eq!(p.taking.len(), 1);
    finished(&mut p, &mut h, 1, true, json!({"files": ["/a.jpg"]}));
    assert_eq!(p.photos, 1);
}

#[test]
fn a_preset_brings_back_the_settings_of_the_trap() {
    let (mut p, mut h, _) = armed();
    p.on_event(&mut h, &pick([0.1, 0.1], [0.5, 0.6]));
    click(&mut p, &mut h, "sensitivity-moved", json!(8.0));
    click(&mut p, &mut h, "count-moved", json!(5.0));
    click(&mut p, &mut h, "pause-moved", json!(30.0));
    click(&mut p, &mut h, "night-toggled", json!(true));
    let saved = p.preset_config();
    assert!(saved.get("folder").is_none() && saved.get("armed").is_none());
    let (mut q, _) = plugin();
    let mut h = host();
    q.apply_preset(&mut h, &saved).unwrap();
    assert_eq!(q.get_config(), p.get_config());
}

#[test]
fn an_older_preset_without_the_keys_changes_nothing_in_phototrap() {
    let (mut p, _) = plugin();
    let mut h = host();
    p.set_config(&json!({"zones": [[0.1, 0.1, 0.5, 0.6]], "sensitivity": 8, "count": 5, "pause": 30, "night": true})).unwrap();
    let before = p.get_config();
    p.apply_preset(&mut h, &json!({})).unwrap();
    assert_eq!(p.get_config(), before);
    p.apply_preset(&mut h, &json!({"count": 9})).unwrap();
    let after = p.get_config();
    assert_eq!((&after["count"], &after["sensitivity"], &after["zones"]), (&json!(9), &json!(8), &before["zones"]));
}

#[test]
fn recalling_a_preset_never_arms_the_trap() {
    let (mut p, _) = plugin();
    let mut h = host();
    p.apply_preset(&mut h, &json!({"zones": [[0.1, 0.1, 0.5, 0.6]], "sensitivity": 8})).unwrap();
    assert!(!p.armed);
    assert!(watches(&h).is_empty());
}

#[test]
fn a_preset_without_zones_leaves_an_armed_trap_watching_its_zones() {
    let (mut p, mut h, _) = armed();
    p.on_event(&mut h, &pick([0.1, 0.1], [0.5, 0.6]));
    let zones = p.get_config()["zones"].clone();
    assert!(p.preset_config().get("zones").is_some());
    let (empty, _) = plugin();
    assert!(empty.preset_config().get("zones").is_none() && empty.preset_config().get("turn").is_none());
    p.apply_preset(&mut h, &json!({"zones": [], "sensitivity": 8})).unwrap();
    assert_eq!(p.get_config()["zones"], zones);
    assert_eq!(p.get_config()["sensitivity"], json!(8));
    p.apply_preset(&mut h, &json!({"zones": [[0.6, 0.2, 0.9, 0.4]]})).unwrap();
    assert_eq!(p.get_config()["zones"], zones, "an armed trap keeps what it watches");
    assert!(p.armed);
}

#[test]
fn reset_puts_the_settings_back_and_leaves_the_zones_and_the_trap_alone() {
    let (mut p, mut h, _) = armed();
    p.zones = vec![[0.1, 0.1, 0.5, 0.5]];
    p.orientation = Turn { flip_h: true, flip_v: false, rotation: None };
    p.zones_changed(&mut h);
    assert_eq!(card(&h)["reset-shown"], json!(false), "nothing to put back yet");
    click(&mut p, &mut h, "sensitivity-moved", json!(9));
    click(&mut p, &mut h, "count-moved", json!(6));
    click(&mut p, &mut h, "pause-moved", json!(40));
    click(&mut p, &mut h, "night-toggled", json!(true));
    assert_eq!(card(&h)["reset-shown"], json!(true));
    let (zones, turn, folder) = (p.zones.clone(), p.get_config()["turn"].clone(), p.folder.clone());
    h.calls.clear();
    click(&mut p, &mut h, "reset-clicked", json!(null));
    let cfg = p.get_config();
    assert_eq!((cfg["sensitivity"].as_u64(), cfg["count"].as_u64(), cfg["pause"].as_u64(), cfg["night"].as_bool()), (Some(5), Some(3), Some(10), Some(false)));
    assert_eq!(p.zones, zones, "the zones stay");
    assert_eq!(cfg["turn"], turn);
    assert_eq!(p.folder, folder);
    assert!(p.armed, "still armed");
    assert_eq!(watches(&h), ["watch_motion phototrap 1 5"], "watched again at the default, never stopped");
    assert!(!h.calls.iter().any(|c| c.starts_with("watch_motion phototrap 0")));
    let shown = card(&h);
    assert_eq!((shown["reset-shown"].clone(), shown["sensitivity"].clone(), shown["pause"].clone(), shown["night"].clone()), (json!(false), json!(5), json!(10), json!(false)));
}

#[test]
fn a_burst_pending_while_arming_again_is_not_counted_twice() {
    let dir = scratch_dir("phototrap-rescan");
    let (mut p, now) = plugin();
    p.folder = dir.display().to_string();
    p.budget = 10_000;
    let mut h = host();
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(true));
    moved(&mut p, &mut h);
    assert_eq!(p.taking.len(), 1);
    let (a, b) = (dir.join("a.jpg"), dir.join("b.jpg"));
    std::fs::write(&a, vec![0u8; 400]).unwrap();
    std::fs::write(&b, vec![0u8; 400]).unwrap();
    // Off and on again while the burst is pending: the rescan finds its files
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(false));
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(true));
    assert_eq!(p.used, 800);
    let files = json!([a.display().to_string(), b.display().to_string()]);
    finished(&mut p, &mut h, 1, true, json!({"files": files}));
    assert_eq!((p.photos, p.used), (2, 800), "counted once");
    assert!(p.counted.is_empty());
    // A later burst is counted as usual
    let c = dir.join("c.jpg");
    std::fs::write(&c, vec![0u8; 100]).unwrap();
    p.taking.push((RequestId(9), None));
    finished(&mut p, &mut h, 9, true, json!({"files": [c.display().to_string()]}));
    assert_eq!(p.used, 900);
    let _ = now;
}

#[test]
fn arming_again_during_a_download_leaves_out_the_empty_name_and_the_partial_file() {
    let dir = scratch_dir("phototrap-rescan-part");
    let (mut p, _now) = plugin();
    p.folder = dir.display().to_string();
    p.budget = 10_000;
    let mut h = host();
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(true));
    moved(&mut p, &mut h);
    assert_eq!(p.taking.len(), 1);
    let (done, reserved) = (dir.join("a.jpg"), dir.join("b.jpg"));
    std::fs::write(&done, vec![0u8; 400]).unwrap();
    std::fs::write(&reserved, b"").unwrap();
    std::fs::write(dir.join("b.jpg.part"), vec![0u8; 300]).unwrap();
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(false));
    click(&mut p, &mut h, "switch-toggled-by-hand", json!(true));
    assert_eq!(p.used, 400, "only the finished photo");
    assert_eq!(p.counted, HashSet::from([done.clone()]));
    // The download ends: the file is its real size, counted once
    std::fs::remove_file(dir.join("b.jpg.part")).unwrap();
    std::fs::write(&reserved, vec![0u8; 500]).unwrap();
    let files = json!([done.display().to_string(), reserved.display().to_string()]);
    finished(&mut p, &mut h, 1, true, json!({"files": files}));
    assert_eq!((p.photos, p.used), (2, 900));
}

#[test]
fn trap_shot_choices_survive_night_presets_and_reset() {
    let (mut p, _) = plugin();
    let mut h = host();
    h.plugins.clear();
    let mut c = caps();
    c["shot"] = json!({"shutter": {"min_ns": 250_000, "max_ns": 10_000_000_000i64}, "iso": {"min": 100, "max": 800}});
    h.still_caps = Some(c);
    p.set_config(&json!({"shot": {"shutter_s": 0.004, "iso": 800}})).unwrap();
    p.setup(&mut h);
    let saved = p.get_config()["shot"].clone();
    assert_eq!(p.options(p.caps.as_ref().unwrap(), &h)["shot"], saved);
    click(&mut p, &mut h, "night-toggled", json!(true));
    assert_eq!(card(&h)["shot-shutter-enabled"], false);
    assert_eq!(card(&h)["shot-iso-enabled"], false);
    assert!(p.options(p.caps.as_ref().unwrap(), &h).get("shot").is_none());
    click(&mut p, &mut h, "shot-iso-picked", json!(0));
    assert_eq!(p.get_config()["shot"], saved);
    p.apply_preset(&mut h, &json!({"night": false})).unwrap();
    assert_eq!(p.preset_config()["shot"], saved);
    assert_eq!(p.options(p.caps.as_ref().unwrap(), &h)["shot"], saved);
    click(&mut p, &mut h, "reset-clicked", Value::Null);
    assert!(p.get_config().get("shot").is_none());
}

#[test]
fn a_late_trap_restore_failure_survives_the_request_ending() {
    let (mut p, _) = plugin();
    let mut h = host();
    p.on_event(&mut h, &Event::RequestProgress { request: RequestId(99), data: json!({"state": "late", "code": "restore", "error": "Restore failed"}) });
    assert_eq!(p.status(&h), ("Restore failed".into(), "status_err"));
}

#[test]
fn phototrap_use_zoom_defaults_off_and_survives_a_preset() {
    let (mut p, mut h, _) = armed();
    assert_eq!(p.get_config()["use_zoom"], false);
    assert_eq!(card(&h)["use-zoom"], false);
    click(&mut p, &mut h, "use-zoom-toggled", json!(true));
    assert_eq!(card(&h)["use-zoom"], true);
    let preset = p.preset_config();
    assert_eq!(preset["use_zoom"], true);
    let (mut restored, _) = plugin();
    restored.apply_preset(&mut h, &preset).unwrap();
    assert_eq!(restored.options(&serde_json::from_value(caps()).unwrap(), &h)["use_zoom"], true);
    restored.apply_preset(&mut h, &json!({"use_zoom": false})).unwrap();
    assert_eq!(restored.get_config()["use_zoom"], false);
}

#[test]
fn the_camera_choice_clears_a_shot_override_without_the_phones_capabilities() {
    let (mut p, _) = plugin();
    let mut h = host();
    h.plugins.clear();
    h.still_caps = None;
    p.set_config(&json!({"shot": {"shutter_s": 0.004, "iso": 800}})).unwrap();
    p.setup(&mut h);
    assert!(p.caps.is_none());
    click(&mut p, &mut h, "shot-iso-picked", json!(0));
    click(&mut p, &mut h, "shot-shutter-picked", json!(0));
    assert!(p.get_config().get("shot").is_none());
}

#[test]
fn the_card_shows_the_phone_on_screen_not_the_one_the_trap_is_armed_on() {
    let (mut p, mut h, _) = armed();
    moved(&mut p, &mut h);
    let on_its_phone = card(&h);
    assert_eq!((on_its_phone["armed"].clone(), on_its_phone["status-kind"].clone()), (json!(true), json!("status_ok")));
    assert!(on_its_phone["tally"].as_str().unwrap().starts_with("Triggers: 1"));
    // The panels show another phone, which nothing is armed on
    h.focused = Some("other".into());
    p.on_event(&mut h, &Event::SourceSelected { source_id: "other".into() });
    let on_another = card(&h);
    assert_eq!(on_another["armed"], json!(false), "its Armed switch is off");
    assert_eq!((on_another["status"].as_str(), on_another["status-kind"].as_str()), (Some("Not armed"), Some("status_dim")));
    assert_eq!(on_another["tally"], json!(""), "the bursts belong to the other phone");
    // Back on the phone it watches, the card says so again
    h.focused = Some("phone".into());
    p.on_event(&mut h, &Event::SourceSelected { source_id: "phone".into() });
    let back = card(&h);
    assert_eq!((back["armed"].clone(), back["status-kind"].clone()), (json!(true), json!("status_ok")));
    assert!(back["tally"].as_str().unwrap().starts_with("Triggers: 1"));
}

#[test]
fn a_problem_the_trap_ran_into_is_not_shown_on_another_phone() {
    let (mut p, mut h, _) = armed();
    moved(&mut p, &mut h);
    finished(&mut p, &mut h, 1, false, json!({"ok": false, "files": [], "code": "failed", "error": "The phone couldn't take the photos"}));
    assert_eq!(card(&h)["status-kind"], json!("status_err"));
    h.focused = Some("other".into());
    p.on_event(&mut h, &Event::SourceSelected { source_id: "other".into() });
    assert_eq!(card(&h)["status-kind"], json!("status_dim"));
    h.focused = Some("phone".into());
    p.on_event(&mut h, &Event::SourceSelected { source_id: "phone".into() });
    assert_eq!(card(&h)["status-kind"], json!("status_err"), "it is still there on its own phone");
}

/// The settings another phone brings along while the trap is armed on this one.
fn other_phones_settings() -> Value {
    json!({"zones": [[0.0, 0.0, 0.2, 0.2], [0.5, 0.5, 0.9, 0.9], [0.1, 0.6, 0.3, 0.8]], "sensitivity": 9, "count": 7, "pause": 60, "night": true})
}

#[test]
fn an_armed_trap_keeps_its_own_settings_when_another_phone_is_shown() {
    let (mut p, mut h, _) = armed();
    click(&mut p, &mut h, "count-moved", json!(4.0));
    assert_eq!(watches(&h), ["watch_motion phototrap 1 5"]);
    // The panels move to another phone, and its settings are loaded into the plugin
    h.focused = Some("other".into());
    h.live = vec!["phone".into()];
    p.set_config(&other_phones_settings()).unwrap();
    p.on_event(&mut h, &Event::SourceSelected { source_id: "other".into() });
    assert_eq!(watches(&h).last().unwrap().as_str(), "watch_motion phototrap 1 5", "still the zones and sensitivity it was armed with");
    moved(&mut p, &mut h);
    assert_eq!(h.stills_taken.len(), 1);
    assert_eq!(h.stills_taken[0].0["count"], 4, "the burst it was armed with");
    assert!(h.stills_taken[0].0.get("extension").is_none(), "and no Night mode the other phone picked");
    // Back on the phone it watches, its own settings are the ones on the card
    h.focused = Some("phone".into());
    p.set_config(&json!({"count": 4})).unwrap();
    p.on_event(&mut h, &Event::SourceSelected { source_id: "phone".into() });
    assert_eq!(watches(&h).last().unwrap().as_str(), "watch_motion phototrap 1 5");
}
