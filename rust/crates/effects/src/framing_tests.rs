use super::*;
use std::sync::{Arc, Mutex, OnceLock};
use telescope_core::frame::{Frame, StreamFrame};
use telescope_core::testing::FakeHost;
use telescope_core::vision::{self, VisionPack};

const ASPECT: f64 = 16.0 / 9.0;

#[derive(Clone)]
struct Who {
    x: f64,
    y: f64,
    width: f64,
    left_wrist: Option<(f64, f64)>,
    left_elbow: Option<(f64, f64)>,
    hips: bool,
    seen: f64,
}

impl Default for Who {
    fn default() -> Who {
        Who { x: 0.5, y: 0.4, width: 0.1, left_wrist: None, left_elbow: None, hips: true, seen: 0.99 }
    }
}

/// Someone facing the camera, shoulders width apart at height y and their left on the right of the picture; seen is how sure the model is of the arm.
fn person(w: Who) -> Person {
    let Who { x, y, width, .. } = w;
    let mut points = vec![
        ("nose", x, y - 0.12, 0.99), ("left_eye", x + 0.01, y - 0.14, 0.99), ("right_eye", x - 0.01, y - 0.14, 0.99),
        ("left_shoulder", x + width / 2.0, y, 0.99), ("right_shoulder", x - width / 2.0, y, 0.99),
        ("right_wrist", x - width / 2.0, y + 0.3, 0.99),
    ];
    if w.hips {
        points.extend([("left_hip", x + width / 3.0, y + 0.35, 0.9), ("right_hip", x - width / 3.0, y + 0.35, 0.9)]);
    }
    if let Some((px, py)) = w.left_wrist {
        points.push(("left_wrist", px, py, w.seen));
    }
    if let Some((px, py)) = w.left_elbow {
        points.push(("left_elbow", px, py, w.seen));
    }
    Person::new(points)
}

fn at(x: f64) -> Person {
    person(Who { x, ..Who::default() })
}

fn close(a: f64, b: f64, by: f64) -> bool {
    (a - b).abs() <= by
}

#[test]
fn the_box_takes_in_the_head_and_shoulders_and_upper_body_takes_more() {
    let head = target_box(&[at(0.5)], ASPECT, 1.0, &[]).unwrap();
    let whole = target_box(&[at(0.5)], ASPECT, 2.0, &[]).unwrap();
    assert!(head.1 < 0.26 && head.0 < 0.45 && 0.55 < head.2);
    assert!(whole.3 > head.3 && whole.2 - whole.0 > head.2 - head.0);
    assert_eq!(target_box(&[], ASPECT, FRAMING_DEFAULT, &[]), None);
    // No shoulders to go by
    assert_eq!(target_box(&[Person::new([("nose", 0.5, 0.5, 0.99)])], ASPECT, FRAMING_DEFAULT, &[]), None);
}

#[test]
fn framing_goes_smoothly_from_the_face_to_the_whole_body() {
    let mut feet = at(0.5);
    feet.points.insert("left_ankle".into(), (0.53, 0.95, 0.9));
    feet.points.insert("right_ankle".into(), (0.47, 0.95, 0.9));
    let box_at = |f: f64| target_box(std::slice::from_ref(&feet), ASPECT, f, &[]).unwrap();
    let heights: Vec<f64> = (0..31).map(|f| box_at(f as f64 / 10.0).3).collect();
    assert!(heights.windows(2).all(|h| h[0] <= h[1] + 1e-9));
    assert!(heights[0] < 0.4 && 0.75 <= heights[20] && heights[30] > 0.95);
    // The top stays above the head
    assert_eq!(box_at(0.0).1, box_at(3.0).1);
}

#[test]
fn two_people_get_one_box_around_both() {
    let b = target_box(&[at(0.3), at(0.7)], ASPECT, FRAMING_DEFAULT, &[]).unwrap();
    assert!(b.0 < 0.25 && b.2 > 0.75);
}

#[test]
fn rotating_the_view_keeps_the_same_part_of_the_person_in_frame() {
    for rotation in [None, Some(90), Some(180), Some(270)] {
        for hand in [false, true] {
            for (flip_h, flip_v) in [(false, false), (true, false), (false, true), (true, true)] {
                for head in [false, true] {
                    let mut p = person(Who { x: 0.65, left_wrist: Some((0.85, 0.45)), left_elbow: Some((0.75, 0.47)), ..Who::default() });
                    if !head {
                        p.points.retain(|n, _| !["nose", "left_eye", "right_eye"].contains(&n.as_str()));
                    }
                    let hands: &[&str] = if hand { &["left"] } else { &[] };
                    let b = target_box(std::slice::from_ref(&p), ASPECT, FRAMING_DEFAULT, hands).unwrap();
                    let g = Geometry { flip_h, flip_v, rotation, ..Geometry::default() };
                    let rotate = |x: f64, y: f64| view_to_shown(x, y, &g);
                    let corners = [rotate(b.0, b.1), rotate(b.0, b.3), rotate(b.2, b.1), rotate(b.2, b.3)];
                    let least = |f: fn(&(f64, f64)) -> f64| corners.iter().map(f).fold(f64::INFINITY, f64::min);
                    let most = |f: fn(&(f64, f64)) -> f64| corners.iter().map(f).fold(f64::NEG_INFINITY, f64::max);
                    let expected = (least(|p| p.0), least(|p| p.1), most(|p| p.0), most(|p| p.1));
                    let aspect = if matches!(rotation, None | Some(180)) { ASPECT } else { 1.0 / ASPECT };
                    let rotated = p.mapped(&rotate);
                    let got = target_box(std::slice::from_ref(&rotated), aspect, FRAMING_DEFAULT, hands).unwrap();
                    assert!(close(got.0, expected.0, 1e-9) && close(got.1, expected.1, 1e-9) && close(got.2, expected.2, 1e-9) && close(got.3, expected.3, 1e-9),
                            "{rotation:?} {hand} {flip_h} {flip_v} {head}: {got:?} {expected:?}");
                    assert_eq!(hands_out(&[rotated], aspect), ["left"]);
                }
            }
        }
    }
}

#[test]
fn the_window_keeps_the_box_and_stays_in_the_picture() {
    let target = fit((0.4, 0.2, 0.6, 0.5), MAX_ZOOM);
    assert!(close(target.zoom, 1.0 / 0.3, 1e-9) && close(target.x, 0.5, 1e-9));
    let corner = fit((0.0, 0.0, 0.1, 0.1), MAX_ZOOM);
    assert!(corner.zoom == MAX_ZOOM && close(corner.x, 0.125, 1e-9) && close(corner.y, 0.125, 1e-9));
    assert_eq!(fit((-0.2, 0.0, 1.2, 1.0), MAX_ZOOM).zoom, 1.0);
}

#[test]
fn a_hand_held_out_widens_the_box_on_its_side_once_it_counts() {
    let out = person(Who { left_wrist: Some((0.66, 0.45)), left_elbow: Some((0.6, 0.47)), ..Who::default() });
    assert_eq!(hands_out(std::slice::from_ref(&out), ASPECT), ["left"]);
    assert!(hands_out(&[at(0.5)], ASPECT).is_empty());
    // Down by the hip: not showing anything
    assert!(hands_out(&[person(Who { left_wrist: Some((0.75, 0.8)), ..Who::default() })], ASPECT).is_empty());
    let (wide, plain) = (target_box(std::slice::from_ref(&out), ASPECT, FRAMING_DEFAULT, &["left"]).unwrap(), target_box(&[out], ASPECT, FRAMING_DEFAULT, &[]).unwrap());
    assert!(wide.2 > 0.66 && 0.66 > plain.2);
}

#[test]
fn an_arm_reaching_past_the_edge_of_the_picture_counts() {
    // The phone only sends a little more than the framing, so the hand is a guess past the edge, the elbow in view
    let reaching = person(Who { left_wrist: Some((0.69, 0.44)), left_elbow: Some((0.62, 0.46)), seen: 0.3, ..Who::default() });
    assert_eq!(hands_out(&[reaching], ASPECT), ["left"]);
    let elbow_only = person(Who { left_wrist: Some((0.66, 0.42)), left_elbow: Some((0.61, 0.47)), seen: 0.3, ..Who::default() });
    assert_eq!(hands_out(&[elbow_only], ASPECT), ["left"]);
}

#[test]
fn hands_on_hips_or_hanging_down_dont_count() {
    let on_hips = person(Who { left_wrist: Some((0.56, 0.74)), left_elbow: Some((0.62, 0.6)), ..Who::default() });
    assert!(hands_out(&[on_hips], ASPECT).is_empty());
    let hanging = person(Who { left_wrist: Some((0.57, 0.7)), left_elbow: Some((0.56, 0.55)), ..Who::default() });
    assert!(hands_out(std::slice::from_ref(&hanging), ASPECT).is_empty());
    // A hand down by the side doesn't stretch the framing to it
    assert!(target_box(&[hanging], ASPECT, FRAMING_DEFAULT, &["left"]).unwrap().3 < 0.6);
}

#[test]
fn the_hand_latch_waits_before_switching_both_ways() {
    let mut latch = Latch::new(0.3, 0.8);
    assert!(!latch.update(true, 0.0) && !latch.update(true, 0.2));
    assert!(latch.update(true, 0.35));
    assert!(latch.update(false, 0.4) && latch.update(false, 1.0));
    assert!(!latch.update(false, 1.25));
}

#[test]
fn small_moves_stay_put_and_a_steady_move_is_followed_without_hops() {
    let mut goal = Target::new(0.5, 0.5, 2.0);
    assert_eq!(follow(goal, Target::new(0.51, 0.5, 2.1), 1.0), goal);
    let (mut xs, mut target) = (Vec::new(), 0.5);
    for _ in 0..60 {
        target += 0.005;
        goal = follow(goal, Target::new(target, 0.5, 2.0), 1.0);
        xs.push(goal.x);
    }
    // It never catches up in one go
    assert!(*xs.last().unwrap() > 0.75 && xs.windows(2).all(|x| x[1] - x[0] <= 0.005 + 1e-9));
    assert!(close(follow(Target::new(0.5, 0.5, 2.0), Target::new(0.5, 0.5, 3.0), 1.0).zoom, 3.0 / 1.12, 1e-9));
}

#[test]
fn smoothing_takes_the_wobble_out_of_the_target() {
    let mut last = None;
    for i in 0..30 {
        last = Some(smoothed(last, Target::new(0.5 + if i % 2 == 1 { 0.02 } else { -0.02 }, 0.5, 2.0), 1.0 / 30.0, 0.3));
    }
    assert!((last.unwrap().x - 0.5).abs() < 0.005);
}

#[test]
fn speed_goes_from_calm_and_patient_to_quick_and_close() {
    let (calm, middle, quick) = (tuning(1), tuning(5), tuning(10));
    assert!(calm.0 < middle.0 && middle.0 < quick.0);
    // A calmer framing lets you move further before it follows
    assert!(calm.2 > middle.2 && middle.2 > quick.2);
    assert!(close(middle.0, 1.6, 1.6 * 0.05));
}

#[test]
fn the_spring_settles_without_overshooting() {
    let mut spring = Spring::new(Target::new(0.5, 0.5, 1.0));
    let goal = Target::new(0.7, 0.4, 3.0);
    let xs: Vec<f64> = (0..150).map(|_| spring.step(goal, 1.0 / 30.0, 4.0).x).collect();
    assert!(xs.windows(2).all(|x| x[0] <= x[1] + 1e-9) && xs.iter().all(|x| *x <= 0.7 + 1e-6));
    assert!(close(spring.at.x, 0.7, 1e-3) && close(spring.at.zoom, 3.0, 1e-2));
}

struct Rig {
    plugin: FramingPlugin,
    host: FakeHost,
    clock: Arc<Mutex<f64>>,
}

impl Rig {
    fn new() -> Rig {
        let clock = Arc::new(Mutex::new(10.0));
        let time = clock.clone();
        let mut rig = Rig { plugin: FramingPlugin::with_clock(move || *time.lock().unwrap()), host: FakeHost::new(), clock };
        rig.host.focused = Some("phone".into());
        rig.host.vision = Some(vision::READY.into());
        rig.host.aspect = 16.0 / 9.0;
        rig.plugin.setup(&mut rig.host);
        rig.configure(&json!({}));
        rig.event(Event::PhonesChanged { count: 1 });
        rig
    }

    /// set_config and the next hook with a host, which is when Python's set_config has done its work.
    fn configure(&mut self, cfg: &Value) {
        self.plugin.set_config(cfg).unwrap();
        self.plugin.on_plugins_ready(&mut self.host);
    }

    fn event(&mut self, event: Event) {
        self.plugin.on_event(&mut self.host, &event);
    }

    fn ui(&mut self, callback: &str, arg: Value) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args: vec![arg] });
    }

    fn switch(&mut self, on: bool) {
        self.ui("switch-toggled-by-hand", json!(on));
    }

    fn tick(&mut self) {
        self.event(Event::TimerFired { owner: NAME.into(), token: T_TICK.into() });
    }

    fn people(&mut self, source: &str, people: &[Person]) {
        self.event(Event::PeopleSeen { source_id: source.into(), people: people.iter().map(Person::to_value).collect() });
    }

    fn run(&mut self, seconds: f64, people: Option<&[Person]>) {
        for _ in 0..(seconds * 30.0).round() as usize {
            *self.clock.lock().unwrap() += 1.0 / 30.0;
            if let Some(people) = people {
                self.people("phone", people);
            }
            self.tick();
        }
    }

    fn last(&self) -> (f32, f32, f32) {
        *self.host.steered.last().expect("it steered")
    }

    /// What the card last heard for this property.
    fn prop(&self, name: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|c| serde_json::from_str::<Value>(c).ok())
            .find_map(|p| p.get(name).cloned()).unwrap_or(Value::Null)
    }

    fn wants_people(&self) -> bool {
        self.host.calls.iter().rev().find_map(|c| c.strip_prefix("want_people Auto framing ")).is_some_and(|on| on == "true")
    }
}

#[test]
fn switching_on_takes_the_view_and_switching_off_gives_it_back() {
    let mut r = Rig::new();
    assert!(r.host.view_owner.is_empty() && !r.wants_people());
    r.switch(true);
    assert!(r.host.view_owner == "Auto framing" && r.wants_people() && r.host.is_scheduled(T_TICK));
    r.switch(false);
    assert!(r.host.view_owner.is_empty() && !r.wants_people() && !r.host.is_scheduled(T_TICK));
}

#[test]
fn it_frames_the_person_on_the_shown_stream_and_goes_wide_when_they_leave() {
    let mut r = Rig::new();
    r.switch(true);
    r.people("another phone", &[at(0.2)]);
    r.run(3.0, Some(&[at(0.6)]));
    let (zoom, pan_x, _) = r.last();
    assert!(zoom > 2.0 && pan_x > 0.0);
    r.host.view = View { zoom, pan_x, ..View::default() };
    // Nobody seen since
    r.run(10.0, None);
    assert!(close(r.last().0 as f64, 1.0, 0.02));
}

#[test]
fn a_flipped_view_pans_the_other_way_to_the_same_person() {
    let mut r = Rig::new();
    r.host.view = View { flip_h: true, ..View::default() };
    r.switch(true);
    r.run(3.0, Some(&[at(0.6)]));
    assert!(r.last().1 < 0.0);
}

#[test]
fn rotating_while_tracking_does_not_steer_to_the_old_screen_coordinates() {
    for rotation in [90, 180, 270] {
        let mut r = Rig::new();
        r.configure(&json!({"on": true, "room": 0, "speed": 10}));
        // Each steer moves the view and says so, as the real host does
        let drive = |r: &mut Rig, seconds: f64, people: Option<&[Person]>| {
            for _ in 0..(seconds * 30.0).round() as usize {
                let count = r.host.steered.len();
                r.run(1.0 / 30.0, people);
                if r.host.steered.len() > count {
                    let (zoom, pan_x, pan_y) = r.last();
                    r.host.view = View { zoom, pan_x, pan_y, ..r.host.view.clone() };
                    r.event(Event::ViewChanged { view: r.host.view.clone() });
                }
            }
        };
        drive(&mut r, 5.0, Some(&[at(0.65)]));
        let before = r.host.view.clone();
        r.host.view = View { rotation: Some(rotation), ..before.clone() };
        r.host.aspect = if rotation == 180 { 16.0 / 9.0 } else { 9.0 / 16.0 };
        r.event(Event::ViewChanged { view: r.host.view.clone() });
        // Until the next pose arrives, the same sensor region should simply turn with the picture
        drive(&mut r, 0.5, None);
        assert!(close(r.host.view.zoom as f64, before.zoom as f64, 0.01), "{rotation}");
        assert!(close(r.host.view.pan_x as f64, before.pan_x as f64, 0.01), "{rotation}");
        assert!(close(r.host.view.pan_y as f64, before.pan_y as f64, 0.01), "{rotation}");
    }
}

#[test]
fn it_says_so_when_something_else_has_the_view() {
    let mut r = Rig::new();
    r.host.view_owner = "Something".into();
    r.switch(true);
    assert!(r.host.view_owner == "Something" && !r.wants_people());
    assert!(r.prop("vision-text").as_str().unwrap().contains("Something else"));
    // The switch stays on, and the reason stays through what else the status row hears
    assert_eq!(r.plugin.get_config()["on"], true);
    r.event(Event::VisionStateChanged { state: "ready".into() });
    assert!(r.prop("vision-text").as_str().unwrap().contains("Something else"));
}

#[test]
fn settings_round_trip_and_damaged_ones_fall_back() {
    let mut r = Rig::new();
    r.configure(&json!({"on": true, "framing": 2.5, "speed": 9, "room": 0, "hand": false}));
    assert_eq!(r.plugin.get_config(), json!({"on": true, "framing": 2.5, "speed": 9, "room": 0, "hand": false}));
    assert_eq!(r.prop("room-text"), "Off");
    assert_eq!(r.prop("framing-text"), "Waist");
    assert_eq!(r.host.view_owner, "Auto framing");
    r.configure(&json!({"on": "yes", "framing": "head", "speed": 30, "room": true}));
    assert_eq!(r.plugin.get_config(), json!({"on": false, "framing": 1.0, "speed": 5, "room": 2, "hand": false}));
    assert_eq!((r.prop("room-text"), r.prop("speed-text"), r.prop("framing-text")), (json!("Shift"), json!("Easy"), json!("Chest")));
    assert_eq!(r.host.view_owner, "");
}

#[test]
fn shutting_down_gives_the_view_back() {
    let mut r = Rig::new();
    r.switch(true);
    r.plugin.shutdown(&mut r.host);
    assert_eq!(r.host.view_owner, "");
}

fn right_edge(steered: (f32, f32, f32)) -> f64 {
    let (zoom, pan_x) = (steered.0 as f64, steered.1 as f64);
    window_centre(zoom, pan_x) + 0.5 / zoom
}

#[test]
fn holding_a_hand_out_gets_it_in_the_picture_quickly_even_at_the_calmest_speed() {
    let mut r = Rig::new();
    r.switch(true);
    // Off by default
    r.ui("hand-toggled", json!(true));
    r.ui("speed-moved", json!(1.0));
    r.run(20.0, Some(&[at(0.5)]));
    assert!(right_edge(r.last()) < 0.69);
    r.run(1.2, Some(&[person(Who { left_wrist: Some((0.69, 0.44)), left_elbow: Some((0.62, 0.46)), seen: 0.3, ..Who::default() })]));
    // The hand and a bit of room for what's in it
    assert!(right_edge(r.last()) > 0.72);
}

fn centre(steered: (f32, f32, f32)) -> f64 {
    window_centre(steered.0 as f64, steered.1 as f64)
}

fn same_place(a: (f32, f32, f32), b: (f32, f32, f32)) -> bool {
    (centre(a) - centre(b)).abs() < 1e-3 && (a.0 - b.0).abs() < 1e-2
}

#[test]
fn shifting_in_your_seat_doesnt_move_the_framing_but_moving_away_does() {
    let mut r = Rig::new();
    r.switch(true);
    r.run(20.0, Some(&[at(0.5)]));
    let settled = r.last();
    // Settling in the chair
    r.run(3.0, Some(&[at(0.52)]));
    assert!(same_place(r.last(), settled));
    // A quick lean to the side
    r.run(0.3, Some(&[at(0.62)]));
    r.run(3.0, Some(&[at(0.5)]));
    assert!(same_place(r.last(), settled));
    // Moving over for good
    r.run(8.0, Some(&[at(0.62)]));
    assert!(close(centre(r.last()), 0.62, 0.01));
    let moved = r.last();
    // And it holds still again where it settled
    r.run(3.0, Some(&[at(0.6)]));
    assert!(same_place(r.last(), moved));
}

#[test]
fn with_no_room_to_move_it_follows_every_shift() {
    let mut r = Rig::new();
    r.switch(true);
    r.ui("room-moved", json!(0.0));
    r.run(20.0, Some(&[at(0.5)]));
    let before = centre(r.last());
    r.run(8.0, Some(&[at(0.52)]));
    assert!(centre(r.last()) > before + 0.01);
}

#[test]
fn a_device_change_goes_through_off_and_starts_from_where_the_view_is() {
    let mut r = Rig::new();
    r.switch(true);
    r.run(3.0, Some(&[at(0.6)]));
    let taken = r.host.calls.iter().filter(|c| c.starts_with("take_view")).count();
    // Back to the defaults, then the next device's saved settings, with no host in between
    r.plugin.set_config(&json!({"on": false, "framing": 1.0, "speed": 5, "room": 2, "hand": false})).unwrap();
    r.plugin.set_config(&json!({"on": true})).unwrap();
    r.event(Event::DeviceChanged { source_id: "B".into() });
    assert!(r.host.calls.iter().any(|c| c == "release_view Auto framing"));
    assert_eq!(r.host.calls.iter().filter(|c| c.starts_with("take_view")).count(), taken + 1);
    assert_eq!(r.host.view_owner, "Auto framing");
    assert!(r.plugin.spring.is_none(), "it starts again from the view");
}

#[test]
fn a_device_without_it_lets_go_at_the_next_hook() {
    let mut r = Rig::new();
    r.switch(true);
    r.plugin.set_config(&json!({"on": false})).unwrap();
    // A tick that's already due doesn't steer the next device
    let count = r.host.steered.len();
    r.tick();
    assert_eq!(r.host.steered.len(), count);
    assert_eq!(r.host.view_owner, "");
}

#[test]
fn the_max_zoom_setting_can_only_lower_it() {
    let mut r = Rig::new();
    r.event(Event::MaxZoomChanged { max_zoom: 2 });
    assert_eq!(r.plugin.max_zoom, 2.0);
    r.event(Event::MaxZoomChanged { max_zoom: 10 });
    assert_eq!(r.plugin.max_zoom, MAX_ZOOM);
}

#[test]
fn a_people_list_from_before_the_view_turned_moves_with_it() {
    let mut r = Rig::new();
    r.switch(true);
    r.people("phone", &[at(0.6)]);
    r.event(Event::ViewChanged { view: View { flip_h: true, ..View::default() } });
    let nose = r.plugin.people[0].point("nose", SEEN).unwrap();
    assert!(close(nose.0, 0.4, 1e-12));
}

#[test]
fn the_status_row_offers_the_download_and_says_how_far_it_got() {
    let mut r = Rig::new();
    r.host.vision = Some(vision::MISSING.into());
    r.host.pack = VisionPack { available: true, size_mb: 47, ..VisionPack::default() };
    r.switch(true);
    assert_eq!((r.prop("vision-text"), r.prop("vision-download")), (json!("Needs a one-time download (about 47 MB)."), json!(true)));
    r.ui("download-clicked", Value::Null);
    assert!(r.host.calls.iter().any(|c| c == "download_vision"));
    r.host.pack = VisionPack { downloading: true, done: 10, size: 40, ..r.host.pack.clone() };
    r.event(Event::VisionDownloadProgress { done: 10, size: 40 });
    assert_eq!((r.prop("vision-text"), r.prop("vision-download")), (json!("Downloading… 25%"), json!(false)));
    r.host.vision = Some(vision::LOADING.into());
    r.host.pack = VisionPack::default();
    r.event(Event::VisionStateChanged { state: vision::LOADING.into() });
    assert_eq!(r.prop("vision-text"), "Getting ready…");
    r.switch(false);
    assert_eq!(r.prop("vision-text"), "");
}

#[test]
fn the_shortcuts_do_what_the_card_does_and_wait_for_a_phone() {
    let mut r = Rig::new();
    let ids: Vec<String> = r.plugin.actions().into_iter().map(|a| a.id).collect();
    assert_eq!(ids, ["framing.on", "framing.framing", "framing.speed"]);
    let run = |r: &mut Rig, id: &str, params: Value| r.plugin.run_action(&mut r.host, id, &params);
    assert_eq!(run(&mut r, "framing.on", json!({})).as_deref(), Some("Auto framing: on"));
    assert_eq!(r.host.view_owner, "Auto framing");
    assert_eq!(run(&mut r, "framing.framing", json!({"op": "up", "amount": 1.0})).as_deref(), Some("Auto framing: how much of you: 11"));
    assert_eq!(r.plugin.get_config()["framing"], 1.1);
    assert_eq!(run(&mut r, "framing.framing", json!({"op": "set", "value": 99})).as_deref(), Some("Auto framing: how much of you: 30"));
    assert_eq!(run(&mut r, "framing.framing", json!({"op": "reset"})).as_deref(), Some("Auto framing: how much of you: 10"));
    assert_eq!(run(&mut r, "framing.speed", json!({"op": "up", "amount": 1.0})).as_deref(), Some("Auto framing speed: Steady"));
    assert_eq!(run(&mut r, "framing.speed", json!({"op": "reset"})).as_deref(), Some("Auto framing speed: Easy"));
    assert_eq!(run(&mut r, "framing.speed", json!({"op": "set"})), None);
    r.plugin.release_action(&mut r.host, "framing.on", &json!({"mode": "toggle"}));
    assert_eq!(run(&mut r, "framing.on", json!({"mode": "hold"})).as_deref(), Some("Auto framing: off"));
    r.plugin.release_action(&mut r.host, "framing.on", &json!({"mode": "hold"}));
    assert_eq!(r.plugin.get_config()["on"], true);
    r.event(Event::PhonesChanged { count: 0 });
    assert_eq!(r.prop("active"), false);
    assert_eq!(run(&mut r, "framing.on", json!({})), None);
    assert_eq!(run(&mut r, "framing.speed", json!({"op": "up", "amount": 1.0})), None);
}

#[test]
fn a_preset_takes_effect_at_once() {
    let mut r = Rig::new();
    r.plugin.apply_preset(&mut r.host, &json!({"on": true, "speed": 2})).unwrap();
    assert_eq!(r.host.view_owner, "Auto framing");
    assert_eq!(r.prop("speed-text"), "Lazy");
}

#[test]
fn it_says_what_it_is_as_python_does() {
    let r = Rig::new();
    let info = r.plugin.info();
    assert_eq!((info.name.as_str(), info.title.as_str(), info.category.as_str(), info.icon.as_str()), ("framing", "Auto framing", "vision", "framing"));
    assert!(info.optional && info.needs_vision && info.in_presets && info.device_local);
    assert_eq!(r.plugin.diagnostics(), [("Auto framing".to_string(), "off".to_string())]);
}

fn fixture() -> &'static Value {
    static F: OnceLock<Value> = OnceLock::new();
    F.get_or_init(|| {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/framing.json")).expect("framing.json");
        serde_json::from_str(&text).expect("json")
    })
}

fn list(key: &str) -> &'static Vec<Value> {
    fixture()[key].as_array().expect("a list")
}

fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("a number, not {v}"))
}

fn target_of(v: &Value) -> Target {
    Target::new(num(&v[0]), num(&v[1]), num(&v[2]))
}

fn near_target(a: Target, b: Target, what: &str) {
    assert!(close(a.x, b.x, 1e-12) && close(a.y, b.y, 1e-12) && close(a.zoom, b.zoom, 1e-11), "{what}: {a:?} {b:?}");
}

#[test]
fn the_box_and_the_hands_match_python_on_every_pose() {
    for case in list("boxes") {
        let people: Vec<Person> = case["people"].as_array().unwrap().iter().map(|p| Person::from_value(p).unwrap()).collect();
        let (aspect, framing) = (num(&case["aspect"]), num(&case["framing"]));
        let hands: Vec<&str> = case["hands"].as_array().unwrap().iter().map(|h| h.as_str().unwrap()).collect();
        let got = target_box(&people, aspect, framing, &hands);
        match (got, case["box"].as_array()) {
            (None, None) => {}
            (Some(g), Some(b)) => {
                let want = [num(&b[0]), num(&b[1]), num(&b[2]), num(&b[3])];
                assert!([g.0, g.1, g.2, g.3].iter().zip(want).all(|(g, w)| close(*g, w, 1e-11)), "{case} {g:?}");
            }
            (g, _) => panic!("{case} {g:?}"),
        }
        let mut out = hands_out(&people, aspect);
        out.sort_unstable();
        assert_eq!(serde_json::to_value(&out).unwrap(), case["out"], "{case}");
    }
}

#[test]
fn the_window_follow_and_smoothing_match_python() {
    for s in list("tuning") {
        let (omega, tau, scale) = tuning(s[0].as_i64().unwrap());
        assert!(close(omega, num(&s[1]), 1e-12) && close(tau, num(&s[2]), 1e-12) && close(scale, num(&s[3]), 1e-12), "{s}");
    }
    for c in list("fits") {
        let b = &c["box"];
        near_target(fit((num(&b[0]), num(&b[1]), num(&b[2]), num(&b[3])), num(&c["max_zoom"])), target_of(&c["at"]), "fit");
    }
    for c in list("follows") {
        near_target(follow(target_of(&c["goal"]), target_of(&c["target"]), num(&c["scale"])), target_of(&c["at"]), "follow");
    }
    for c in list("insides") {
        assert_eq!(inside(target_of(&c["anchor"]), target_of(&c["target"]), num(&c["room"])), c["in"], "{c}");
    }
    for c in list("smooths") {
        near_target(smoothed(Some(target_of(&c["last"])), target_of(&c["target"]), num(&c["dt"]), num(&c["tau"])), target_of(&c["at"]), "smoothed");
    }
}

#[test]
fn the_spring_latch_and_room_match_python_step_by_step() {
    let mut spring = Spring::new(Target::new(0.5, 0.5, 1.0));
    for c in list("springs") {
        near_target(spring.step(target_of(&c["goal"]), num(&c["dt"]), num(&c["omega"])), target_of(&c["at"]), "spring");
    }
    let mut latch = Latch::new(0.3, 0.8);
    for c in list("latches") {
        assert_eq!(latch.update(c["value"].as_bool().unwrap(), num(&c["now"])), c["on"], "{c}");
    }
    let mut room = Room::default();
    // The clock adds up as it did in Python: a time read back from JSON can land the other side of LEAVE_S
    let mut now = 0.0;
    for c in list("rooms") {
        now += 1.0 / 30.0;
        let held = room.goal(target_of(&c["target"]), target_of(&c["at"]), 0.12, now);
        match (held, c["held"].is_null()) {
            (None, true) => {}
            (Some(h), false) => near_target(h, target_of(&c["held"]), "room"),
            (h, _) => panic!("{c} {h:?}"),
        }
    }
}

#[test]
fn settings_load_as_python_loads_them() {
    let mut r = Rig::new();
    for c in list("configs") {
        r.configure(&c["in"]);
        assert_eq!(r.plugin.get_config(), c["out"], "{c}");
        assert_eq!((r.prop("framing-text"), r.prop("speed-text"), r.prop("room-text")), (c["framing"].clone(), c["speed"].clone(), c["room"].clone()), "{c}");
    }
}

fn view_of(v: &Value) -> View {
    View {
        zoom: num(&v["zoom"]) as f32, pan_x: num(&v["pan_x"]) as f32, pan_y: num(&v["pan_y"]) as f32,
        flip_h: v["flip_h"].as_bool().unwrap(), flip_v: v["flip_v"].as_bool().unwrap(),
        rotation: v["rotation"].as_i64().map(|d| d as i32), ..View::default()
    }
}

/// The plugin as a whole steers the same on every tick of Python's runs, through the same people, turns and settings.
#[test]
fn whole_runs_steer_as_python_does_tick_by_tick() {
    for run in list("runs") {
        let name = run["name"].as_str().unwrap();
        let mut r = Rig::new();
        r.host.view = view_of(&run["view"]);
        r.host.view_owner = run["owner"].as_str().unwrap().into();
        r.plugin.setup(&mut r.host);
        r.configure(&run["config"]);
        for (i, op) in run["ops"].as_array().unwrap().iter().enumerate() {
            match op["op"].as_str().unwrap() {
                "clock" => *r.clock.lock().unwrap() += num(&op["dt"]),
                "tick" => {
                    *r.clock.lock().unwrap() += num(&op["dt"]);
                    let count = r.host.steered.len();
                    r.tick();
                    let steered = (r.host.steered.len() > count).then(|| r.last());
                    match (steered, op["steered"].as_array()) {
                        (None, None) => {}
                        (Some(s), Some(w)) => assert!(close(s.0 as f64, num(&w[0]), 1e-6) && close(s.1 as f64, num(&w[1]), 1e-6) && close(s.2 as f64, num(&w[2]), 1e-6),
                                                      "{name} op {i}: {s:?} {w:?}"),
                        (s, w) => panic!("{name} op {i}: {s:?} {w:?}"),
                    }
                }
                "people" => {
                    let people: Vec<Person> = op["people"].as_array().unwrap().iter().map(|p| Person::from_value(p).unwrap()).collect();
                    r.people(op["source"].as_str().unwrap(), &people);
                }
                "view" => {
                    r.host.view = view_of(&op["view"]);
                    if let Some(aspect) = op["aspect"].as_f64() {
                        r.host.aspect = aspect as f32;
                    }
                    r.event(Event::ViewChanged { view: r.host.view.clone() });
                }
                "max_zoom" => r.event(Event::MaxZoomChanged { max_zoom: op["most"].as_i64().unwrap() as i32 }),
                "focus" => r.plugin.on_focus_gained(&mut r.host, &StreamInfo { source_id: "phone".into(), url: "url".into(), session: 0 }),
                "config" => r.configure(&op["config"]),
                other => panic!("{other}"),
            }
        }
        let end = &run["end"];
        assert_eq!(r.plugin.get_config(), end["config"], "{name}");
        assert_eq!(r.host.view_owner, end["owner"].as_str().unwrap(), "{name}");
        assert_eq!(r.wants_people(), !end["wanted"].as_array().unwrap().is_empty(), "{name}");
        assert_eq!(r.prop("vision-text"), end["status"], "{name}");
        assert_eq!(r.plugin.diagnostics()[0].1, end["diagnostics"]["Auto framing"].as_str().unwrap(), "{name}");
    }
}

// ── The bubbles ───────────────────────────────────────────────────────

impl Rig {
    fn peek_event(&mut self, token: &str) {
        self.event(Event::TimerFired { owner: NAME.into(), token: token.into() });
    }

    /// A timer of the Speed bubble `ms` after the last one.
    fn chase_after(&mut self, ms: u64) {
        *self.clock.lock().unwrap() += ms as f64 / 1000.0;
        self.peek_event(T_CHASE);
    }

    fn pictures(&self) -> usize {
        self.host.calls.iter().filter(|c| c.starts_with("set_ui_image card framing-picture")).count()
    }

    fn dots(&self) -> Vec<f64> {
        self.prop("speed-dots").as_array().map(|a| a.iter().map(|v| v.as_f64().unwrap()).collect()).unwrap_or_default()
    }

    /// The part of the picture the Framing bubble shows now: x, y, width, height as fractions of it.
    fn clip(&self) -> Vec<f64> {
        self.prop("framing-clip").as_array().map(|a| a.iter().map(|v| v.as_f64().unwrap()).collect()).unwrap_or_default()
    }
}

#[test]
fn the_framing_bubble_shows_the_crop_unless_the_preview_already_shows_it() {
    let mut r = Rig::new();
    assert_eq!(r.prop("framing-peek"), json!("picture"), "off: the preview shows nothing of it");
    r.switch(true);
    assert_eq!(r.prop("framing-peek"), json!("picture"), "on, but nobody to follow yet");
    r.run(1.0, Some(&[at(0.6)]));
    assert_eq!(r.prop("framing-peek"), json!(""), "following someone: the preview shows it");
    r.run(10.0, None);
    assert_eq!(r.prop("framing-peek"), json!("picture"), "nobody seen for a while: back to the whole picture");
    r.run(1.0, Some(&[at(0.6)]));
    assert_eq!(r.prop("framing-peek"), json!(""));
    r.switch(false);
    assert_eq!(r.prop("framing-peek"), json!("picture"));
    // something else holds the view: nothing is followed
    let mut other = Rig::new();
    other.host.view_owner = "Transforms".into();
    other.switch(true);
    other.run(1.0, Some(&[at(0.6)]));
    assert_eq!(other.prop("framing-peek"), json!("picture"));
}

#[test]
fn the_card_starts_out_with_the_bubble_it_will_have() {
    let mut plugin = FramingPlugin::new();
    assert_eq!(plugin.card().unwrap().ui.props["framing-peek"], json!("picture"));
}

#[test]
fn the_framing_bubble_costs_nothing_while_closed_and_hands_over_a_picture_once_then_only_crops_it() {
    let mut r = Rig::new();
    assert!(!r.host.is_scheduled(T_PEEK) && r.pictures() == 0);
    // a stray timer while closed makes nothing
    r.peek_event(T_PEEK);
    assert_eq!(r.pictures(), 0);
    r.ui("framing-peek-open", json!(true));
    assert!(r.host.scheduled.contains(&(peek::PICTURE_MS, T_PEEK.into())) && r.pictures() == 1);
    assert_eq!(peek::PICTURE_MS, 125);
    assert!(r.host.calls.iter().any(|c| c == "want_people Auto framing bubble true"), "the people are asked for while it is up");
    let picture = &r.host.images[&("card".to_string(), "framing-picture".to_string())];
    assert_eq!((picture.width as usize, picture.height as usize), (peek::SCENE_WIDTH, peek::SCENE_HEIGHT));
    assert_eq!(r.clip().len(), 4);
    // nothing changed: nothing is handed over again, not the picture and not the crop
    let calls = r.host.calls.len();
    r.peek_event(T_PEEK);
    assert_eq!(r.pictures(), 1, "the photo is one picture the bubble crops");
    assert_eq!(r.host.calls.len(), calls, "nothing is asked of the host: {:?}", &r.host.calls[calls..]);
    r.ui("framing-peek-open", json!(false));
    assert!(!r.host.is_scheduled(T_PEEK));
    assert!(r.host.calls.iter().any(|c| c == "want_people Auto framing bubble false"));
    r.peek_event(T_PEEK);
    assert_eq!(r.pictures(), 1, "closed: nothing more is made");
    // opened again, it hands the picture over once more
    r.ui("framing-peek-open", json!(true));
    assert_eq!(r.pictures(), 2);
}

fn left_red_right_blue() -> StreamFrame {
    let (w, h) = (320u32, 180u32);
    let data = (0..w * h).flat_map(|i| if (i % w) < w / 2 { [255, 0, 0, 255] } else { [0, 0, 255, 255] }).collect();
    StreamFrame { source_id: "phone".into(), name: "phone".into(), frame: Frame::new(w, h, telescope_core::frame::PixelFormat::Rgba, data) }
}

#[test]
fn the_bubble_crops_the_live_picture_around_whoever_is_in_it_and_a_stand_in_scene_when_nobody_is() {
    let mut r = Rig::new();
    r.host.stream_frames = vec![left_red_right_blue()];
    r.ui("framing-peek-open", json!(true));
    let key = ("card".to_string(), "framing-picture".to_string());
    let scene = r.host.images[&key].clone();
    // someone on the right: the live frame is the picture and the crop is over the blue half
    r.people("phone", &[at(0.8)]);
    r.peek_event(T_PEEK);
    let live = r.host.images[&key].clone();
    assert_eq!((live.width, live.height), (320, 180), "the frame as it is, already small enough");
    assert_ne!(scene.data, live.data, "the stand-in is another picture");
    let clip = r.clip();
    assert!(clip[0] >= 0.5 - 1e-9 && clip[2] < 1.0, "over the blue half: {clip:?}");
    // the same frame again: not handed over twice
    let pictures = r.pictures();
    r.peek_event(T_PEEK);
    assert_eq!(r.pictures(), pictures);
    // a closer framing is still over the blue half, with no new picture
    r.ui("framing-moved", json!(0));
    let closer = r.clip();
    assert!(closer[0] >= 0.5 - 1e-9 && closer[2] < clip[2], "{closer:?} against {clip:?}");
    assert_eq!(r.pictures(), pictures, "a drag only moves the crop");
    // nobody now (seen too long ago): the stand-in again
    *r.clock.lock().unwrap() += 10.0;
    r.peek_event(T_PEEK);
    assert_eq!(r.host.images[&key].data, scene.data, "the wall's colours, not the live picture's");
    assert!(r.pictures() > pictures);
}

#[test]
fn the_photo_crop_is_closer_the_closer_the_framing() {
    let mut r = Rig::new();
    r.ui("framing-peek-open", json!(true));
    let mut widths = Vec::new();
    for steps in [0, 10, 20, 30] {
        r.ui("framing-moved", json!(steps));
        let clip = r.clip();
        // inside the picture and the shape of it
        assert!(clip[0] >= 0.0 && clip[1] >= 0.0 && clip[0] + clip[2] <= 1.0 + 1e-9 && clip[1] + clip[3] <= 1.0 + 1e-9, "{clip:?}");
        assert!((clip[2] - clip[3]).abs() < 1e-9);
        widths.push(clip[2]);
    }
    assert!(widths.windows(2).all(|w| w[0] <= w[1]), "{widths:?}");
    assert!(widths[3] > 2.0 * widths[0], "{widths:?}");
    // and by the photo's own points it holds the head and shoulders at the closest, not the hips, and the feet at the widest
    let person = peek::sample_person();
    let holds = |clip: &[f64], name: &str| {
        let (x, y) = person.point(name, telescope_core::pose::SEEN).unwrap();
        x >= clip[0] && x <= clip[0] + clip[2] && y >= clip[1] && y <= clip[1] + clip[3]
    };
    r.ui("framing-moved", json!(0));
    let close = r.clip();
    assert!(["nose", "left_eye", "right_eye", "left_shoulder", "right_shoulder"].iter().all(|n| holds(&close, n)), "{close:?}");
    assert!(!holds(&close, "left_hip") && !holds(&close, "right_hip"), "{close:?}");
    assert!(close[1] < 0.07, "room above the head: {close:?}");
    r.ui("framing-moved", json!(30));
    let wide = r.clip();
    assert!(["nose", "left_wrist", "right_wrist", "left_knee", "left_ankle", "right_ankle"].iter().all(|n| holds(&wide, n)), "{wide:?}");
}

#[test]
fn the_speed_bubble_moves_its_dots_only_while_it_is_up() {
    let mut r = Rig::new();
    assert!(!r.host.is_scheduled(T_CHASE) && r.dots().is_empty());
    r.peek_event(T_CHASE);
    assert!(r.dots().is_empty(), "closed: a stray timer moves nothing");
    r.ui("speed-peek-open", json!(true));
    assert!(r.host.scheduled.contains(&(CHASE_MS, T_CHASE.into())));
    assert_eq!(CHASE_MS, 16, "as often as the window draws");
    let first = r.dots();
    assert_eq!(first.len(), 8, "you, the frame and its two copies");
    // the very first tick already has you hopping
    r.chase_after(CHASE_MS);
    assert_ne!(first[..2], r.dots()[..2], "you moved on the first tick");
    for _ in 0..90 {
        r.chase_after(CHASE_MS);
    }
    let later = r.dots();
    assert_ne!(first, later, "you moved on your own while it was open");
    r.ui("speed-peek-open", json!(false));
    assert!(!r.host.is_scheduled(T_CHASE));
    let before = r.dots();
    r.chase_after(CHASE_MS);
    assert_eq!(r.dots(), before);
}

#[test]
fn a_late_timer_moves_the_dots_further_instead_of_slowing_them() {
    let (mut steady, mut late) = (Rig::new(), Rig::new());
    for r in [&mut steady, &mut late] {
        r.ui("speed-peek-open", json!(true));
    }
    for _ in 0..40 {
        steady.chase_after(16);
    }
    for _ in 0..20 {
        late.chase_after(32);
    }
    let (a, b) = (steady.dots(), late.dots());
    assert!(a.iter().zip(&b).all(|(x, y)| close(*x, *y, 0.03)), "{a:?} against {b:?}");
}

/// How far the framing is from you at the end of a hop (the next one is about to start), and how far it got from where it was.
fn lag_at(speed: i64) -> (f64, f64) {
    let mut chase = Chase::new();
    let start = chase.step(speed, 0.0);
    let mut last = start;
    for _ in 0..((Chase::MOVE_S + Chase::HOLD_S) / 0.016) as usize {
        last = chase.step(speed, 0.016);
    }
    ((last[0] - last[2]).hypot(last[1] - last[3]), (last[2] - start[2]).hypot(last[3] - start[3]))
}

#[test]
fn the_dots_lag_by_what_the_speed_gives() {
    let lags: Vec<(f64, f64)> = (1..=10).map(lag_at).collect();
    assert!(lags.windows(2).all(|w| w[0].0 >= w[1].0 - 1e-9), "slower speeds trail further behind: {lags:?}");
    // Speed 1 is well short of you when the next hop comes (but has moved), Speed 10 has landed
    assert!(lags[0].0 > 0.25 && lags[0].1 > 0.1, "{lags:?}");
    assert!(lags[9].0 < 0.01, "{lags:?}");
}

/// Seconds after a hop starts until the framing is within a tenth of the hop's length of you.
fn landing_time(speed: i64) -> f64 {
    let mut chase = Chase::new();
    let start = chase.step(speed, 0.0);
    let mut t = 0.0;
    let mut hop = 0.0f64;
    while t < 3.0 {
        let now = chase.step(speed, 0.004);
        t += 0.004;
        hop = hop.max((now[0] - start[0]).hypot(now[1] - start[1]));
        if t > Chase::MOVE_S && (now[0] - now[2]).hypot(now[1] - now[3]) < hop * 0.1 {
            return t;
        }
    }
    t
}

#[test]
fn the_fastest_speed_lands_in_about_a_fifth_of_a_second_and_the_slowest_not_within_a_hop() {
    assert!(landing_time(10) < Chase::MOVE_S + 0.25, "{}", landing_time(10));
    assert!(landing_time(1) > Chase::MOVE_S + Chase::HOLD_S, "{}", landing_time(1));
}

#[test]
fn the_chase_runs_on_the_same_smoothing_and_spring_as_the_framing_without_the_dead_zone() {
    for speed in [1, 5, 10] {
        let mut chase = Chase::new();
        let (omega, tau, _) = tuning(speed);
        let mut spring = Spring::new(Chase::you(Chase::HOLD_S));
        let mut smooth = None;
        let mut now = Chase::HOLD_S;
        for i in 1..=120 {
            now += 0.016;
            let s = smoothed(smooth, Chase::you(now), 0.016 * Chase::RATE, tau);
            smooth = Some(s);
            let at = spring.step(s, 0.016 * Chase::RATE, omega);
            let [_, _, fx, fy, ..] = chase.step(speed, 0.016);
            assert!(close(fx, at.x, 1e-9) && close(fy, at.y, 1e-9), "speed {speed} step {i}");
        }
    }
}

#[test]
fn the_copies_trail_the_framing_by_a_moment_and_huddle_when_it_is_slow() {
    let spread = |speed: i64| {
        let mut chase = Chase::new();
        let mut widest = 0.0f64;
        for _ in 0..200 {
            let [_, _, fx, fy, ax, ay, bx, by] = chase.step(speed, 0.016);
            widest = widest.max((fx - bx).hypot(fy - by));
            assert!((0.0..=1.0).contains(&ax) && (0.0..=1.0).contains(&by));
            let _ = ay;
        }
        widest
    };
    assert!(spread(10) > 3.0 * spread(1), "{} {}", spread(10), spread(1));
    assert!(spread(10) > 0.3, "a quick hop is drawn with room between the copies: {}", spread(10));
}

#[test]
fn the_stand_in_for_you_hops_between_four_spots_across_the_whole_box_and_holds_at_each() {
    let leg = Chase::HOLD_S + Chase::MOVE_S;
    let spots: Vec<Target> = (0..4).map(|i| Chase::you(i as f64 * leg + Chase::HOLD_S * 0.5)).collect();
    for (spot, want) in spots.iter().zip(Chase::SPOTS) {
        assert!(close(spot.x, want.0, 1e-9) && close(spot.y, want.1, 1e-9));
    }
    let xs: Vec<f64> = spots.iter().map(|s| s.x).collect();
    let ys: Vec<f64> = spots.iter().map(|s| s.y).collect();
    let span = |v: &[f64]| v.iter().cloned().fold(f64::MIN, f64::max) - v.iter().cloned().fold(f64::MAX, f64::min);
    assert!(span(&xs) >= 0.8 && span(&ys) >= 0.6, "most of the box: {xs:?} {ys:?}");
    // every hop is a good way across, and the four of them go up as well as down
    for i in 0..4 {
        let (a, b) = (spots[i], spots[(i + 1) % 4]);
        assert!((a.x - b.x).hypot(a.y - b.y) > 0.4, "hop {i}");
    }
    assert!(spots[0].y < spots[1].y && spots[2].y > spots[3].y);
    assert!(close(Chase::you(4.0 * leg).x, spots[0].x, 1e-9) || close(Chase::you(4.0 * leg + Chase::HOLD_S * 0.5).x, spots[0].x, 1e-9));
    assert!((0..800).all(|i| (0.0..=1.0).contains(&Chase::you(i as f64 * 0.02).y) && (0.0..=1.0).contains(&Chase::you(i as f64 * 0.02).x)));
}

#[test]
fn the_first_hop_comes_on_the_first_tick() {
    let mut chase = Chase::new();
    let first = chase.step(5, 0.0);
    let after = chase.step(5, 0.016);
    assert!((after[0] - first[0]).abs() + (after[1] - first[1]).abs() > 1e-4, "you are already moving: {first:?} then {after:?}");
}

#[test]
fn the_chase_keeps_one_tempo_whatever_the_speed_does() {
    // One stand-in that is never touched, one whose Speed is changed on every tick as if the slider were being dragged: you are in the same place throughout
    let (mut quiet, mut dragged) = (Chase::new(), Chase::new());
    for i in 0..600 {
        let a = quiet.step(5, 0.016);
        let b = dragged.step(1 + (i % 10) as i64, 0.016);
        assert!(close(a[0], b[0], 1e-9) && close(a[1], b[1], 1e-9), "tick {i}: you moved differently, {a:?} against {b:?}");
    }
}
