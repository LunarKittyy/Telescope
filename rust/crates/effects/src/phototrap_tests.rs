use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use telescope_core::host::Pick;
use telescope_core::testing::FakeHost;

fn caps() -> Value {
    json!({"burst_max": 8, "extensions": ["night"]})
}

fn host() -> FakeHost {
    let mut h = FakeHost::new();
    h.streaming = true;
    h.still_caps = Some(caps());
    h
}

/// A plugin on a clock the test moves, in seconds.
fn plugin() -> (PhototrapPlugin, Arc<AtomicU64>) {
    let now = Arc::new(AtomicU64::new(100));
    let seen = now.clone();
    let mut p = PhototrapPlugin::with_clock(move || seen.load(Ordering::SeqCst) as f64);
    p.folder = "/photos/Phototrap".into();
    (p, now)
}

fn click(p: &mut PhototrapPlugin, h: &mut FakeHost, callback: &str, arg: Value) {
    p.on_ui(h, &UiEvent { ui: "card".into(), callback: callback.into(), args: vec![arg] });
}

fn moved(p: &mut PhototrapPlugin, h: &mut FakeHost) {
    p.on_event(h, &Event::MotionSeen { source_id: "phone".into(), owner: NAME.into(), zones: vec![0] });
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
    p.on_event(&mut h, &Event::MotionSeen { source_id: "phone".into(), owner: NAME.into(), zones: vec![] });
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
