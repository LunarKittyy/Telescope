use super::*;
use std::sync::{Arc, Mutex, OnceLock};
use telescope_core::host::View;
use telescope_core::testing::FakeHost;
use telescope_core::vision;

fn near(a: f64, b: f64, by: f64) -> bool {
    (a - b).abs() <= by
}

fn at(a: (f64, f64), b: (f64, f64)) -> bool {
    near(a.0, b.0, 1e-6) && near(a.1, b.1, 1e-6)
}

/// Feeds the same face for seconds; returns (time, point) for each move.
fn run(meter: &mut FaceMeter, face: Option<(f64, f64)>, start: f64, seconds: f64) -> Vec<(f64, (f64, f64))> {
    let (mut moves, mut t) = (Vec::new(), start);
    while t < start + seconds - 1e-9 {
        if let Some(point) = meter.update(face, t) {
            moves.push((t, point));
        }
        t += 1.0 / 12.0;
    }
    moves
}

#[test]
fn the_first_move_waits_for_the_face_to_settle() {
    let mut meter = FaceMeter::default();
    let moves = run(&mut meter, Some((0.4, 0.3)), 0.0, 2.0);
    assert!(moves.len() == 1 && moves[0].0 >= SETTLE_S - 1e-9 && at(moves[0].1, (0.4, 0.3)));
}

#[test]
fn small_head_moves_dont_move_the_point() {
    let mut meter = FaceMeter::default();
    run(&mut meter, Some((0.4, 0.3)), 0.0, 2.0);
    for i in 0..100 {
        let face = (0.4 + if i % 2 == 1 { 0.04 } else { -0.04 }, 0.3 + if i % 3 != 0 { 0.03 } else { 0.0 });
        assert_eq!(meter.update(Some(face), 2.0 + i as f64 / 12.0), None);
    }
    assert!(at(meter.point.unwrap(), (0.4, 0.3)));
}

#[test]
fn a_real_move_is_followed_slowly_and_not_too_often() {
    let mut meter = FaceMeter::default();
    run(&mut meter, Some((0.3, 0.4)), 0.0, 2.0);
    let moves = run(&mut meter, Some((0.7, 0.4)), 2.0, 10.0);
    assert!(!moves.is_empty(), "it follows in the end");
    // Not before the face has settled
    assert!(moves[0].0 >= 2.0 + SETTLE_S - 1e-9);
    assert!(moves.windows(2).all(|m| m[1].0 - m[0].0 >= EVERY_S - 1e-9));
    assert!(near(meter.point.unwrap().0, 0.7, DEAD_ZONE));
}

#[test]
fn walking_in_moves_the_point_once_you_settle() {
    let mut meter = FaceMeter::default();
    run(&mut meter, Some((0.4, 0.3)), 0.0, 2.0);
    // Across the room
    for i in 0..15 {
        assert_eq!(meter.update(Some((0.4 + i as f64 * 0.02, 0.3)), 2.0 + i as f64 / 12.0), None);
    }
    let moves = run(&mut meter, Some((0.7, 0.3)), 2.0 + 15.0 / 12.0, 5.0);
    assert!(moves.len() == 1 && near(moves[0].1 .0, 0.7, 0.02));
}

#[test]
fn leaving_the_picture_holds_the_point() {
    let mut meter = FaceMeter::default();
    run(&mut meter, Some((0.4, 0.3)), 0.0, 2.0);
    assert!(run(&mut meter, None, 2.0, 20.0).is_empty());
    assert!(at(meter.point.unwrap(), (0.4, 0.3)));
    // Back in the same chair: nothing to do
    assert!(run(&mut meter, Some((0.42, 0.31)), 22.0, 3.0).is_empty());
}

struct Rig {
    plugin: FaceExposurePlugin,
    host: FakeHost,
    clock: Arc<Mutex<f64>>,
    focus: Vec<(f64, f64)>,
    metered: Vec<(f64, f64)>,
}

impl Rig {
    fn new() -> Rig {
        let clock = Arc::new(Mutex::new(100.0));
        let time = clock.clone();
        let mut r = Rig { plugin: FaceExposurePlugin::with_clock(move || *time.lock().unwrap()), host: FakeHost::new(), clock, focus: Vec::new(), metered: Vec::new() };
        r.host.focused = Some("phone".into());
        r.host.streaming = true;
        r.host.vision = Some(vision::READY.into());
        r.plugin.setup(&mut r.host);
        r.configure(&json!({}));
        r.event(Event::FocusPointAvailable { available: true });
        r.event(Event::PhonesChanged { count: 1 });
        r
    }

    fn configure(&mut self, cfg: &Value) {
        self.plugin.set_config(cfg).unwrap();
        self.plugin.on_plugins_ready(&mut self.host);
    }

    /// What the plugin said goes round the bus and back to it after the hook, as it does in the app.
    fn deliver(&mut self) {
        while !self.host.events.is_empty() {
            let event = self.host.events.remove(0);
            match event {
                Event::FocusPointPicked { x, y } => self.focus.push((x as f64, y as f64)),
                Event::MeterPointPicked { x, y } => self.metered.push((x as f64, y as f64)),
                _ => {}
            }
            self.plugin.on_event(&mut self.host, &event);
        }
    }

    fn event(&mut self, event: Event) {
        self.plugin.on_event(&mut self.host, &event);
        self.deliver();
    }

    fn switch(&mut self, on: bool) {
        self.ui("switch-toggled-by-hand", json!(on));
    }

    fn ui(&mut self, callback: &str, arg: Value) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args: vec![arg] });
        self.deliver();
    }

    fn see_from(&mut self, face: (f64, f64), seconds: f64, source: &str) {
        for _ in 0..(seconds * 12.0).round() as usize {
            *self.clock.lock().unwrap() += 1.0 / 12.0;
            let track = Track { id: 1, bbox: (face.0 - 0.1, face.1 - 0.1, face.0 + 0.1, face.1 + 0.5), face: Some(face), seen_s: 5.0, missing_s: 0.0 };
            self.event(Event::PeopleTracked { source_id: source.into(), tracks: vec![track.to_value()], count: 1 });
        }
    }

    fn see(&mut self, face: (f64, f64)) {
        self.see_from(face, 1.0, "phone");
    }

    fn prop(&self, name: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|c| serde_json::from_str::<Value>(c).ok())
            .find_map(|p| p.get(name).cloned()).unwrap_or(Value::Null)
    }

    fn wants_people(&self) -> bool {
        self.host.calls.iter().rev().find_map(|c| c.strip_prefix("want_people Expose for my face ")).is_some_and(|on| on == "true")
    }

    fn diagnosis(&self) -> String {
        self.plugin.diagnostics()[0].1.clone()
    }
}

#[test]
fn switching_on_asks_for_people_and_meters_on_the_face() {
    let mut r = Rig::new();
    r.see((0.4, 0.3));
    // Off by default
    assert!(r.focus.is_empty() && !r.wants_people());
    r.switch(true);
    assert!(r.wants_people());
    r.see((0.4, 0.3));
    assert!(r.focus.len() == 1 && at(r.focus[0], (0.4, 0.3)));
    r.see_from((0.4, 0.3), 1.0, "other");
    assert_eq!(r.focus.len(), 1);
    r.switch(false);
    assert!(!r.wants_people());
}

#[test]
fn the_point_goes_where_the_face_is_in_the_picture_as_shown() {
    let mut r = Rig::new();
    r.host.view = View { flip_h: true, zoom: 2.0, ..View::default() };
    r.switch(true);
    r.see((0.4, 0.3));
    // People are found in the picture as shown (mirrored too), so only the zoom moves it: twice as far from the middle
    assert!(r.focus.len() == 1 && at(r.focus[0], (0.3, 0.1)));
    r.focus.clear();
    r.plugin.meter.reset();
    // Outside the window shown: no point there
    r.see((0.1, 0.5));
    assert!(r.focus.is_empty());
}

#[test]
fn the_point_follows_a_stretched_picture() {
    let mut r = Rig::new();
    r.host.view = View { stretch_x: 0.5, ..View::default() };
    r.switch(true);
    r.see((0.3, 0.3));
    // Width at 50% moves the face halfway to the middle
    assert!(r.focus.len() == 1 && at(r.focus[0], (0.4, 0.3)));
}

#[test]
fn a_point_picked_by_hand_wins_until_switched_off_and_on() {
    let mut r = Rig::new();
    r.switch(true);
    r.see((0.4, 0.3));
    // Clicked on the preview
    r.event(Event::FocusPointPicked { x: 0.9, y: 0.9 });
    r.focus.clear();
    r.see_from((0.8, 0.3), 5.0, "phone");
    assert!(r.focus.is_empty() && r.prop("held") == true);
    r.switch(false);
    r.switch(true);
    r.see((0.8, 0.3));
    assert!(r.focus.len() == 1 && at(r.focus[0], (0.8, 0.3)));
}

#[test]
fn picking_a_focus_mode_by_hand_wins_too_and_follow_my_face_comes_back() {
    let mut r = Rig::new();
    r.switch(true);
    r.event(Event::FocusModePicked { mode: "auto".into() });
    r.see((0.4, 0.3));
    assert!(r.focus.is_empty());
    r.ui("follow-clicked", Value::Null);
    r.see((0.4, 0.3));
    assert!(r.focus.len() == 1 && at(r.focus[0], (0.4, 0.3)));
}

#[test]
fn picking_manual_by_hand_shows_the_manual_focus_note() {
    let mut r = Rig::new();
    r.switch(true);
    r.event(Event::FocusModePicked { mode: "manual".into() });
    r.event(Event::FocusModeChanged { mode: "manual".into() });
    r.see((0.4, 0.3));
    assert!(r.focus.is_empty() && r.prop("held-note").as_str().unwrap().contains("Manual"));
    assert_eq!(r.diagnosis(), "waiting (manual focus)");
}

#[test]
fn nothing_is_sent_while_the_lens_cant_meter_on_a_point() {
    let mut r = Rig::new();
    r.switch(true);
    r.event(Event::FocusPointAvailable { available: false });
    r.see((0.4, 0.3));
    assert!(r.focus.is_empty() && r.prop("vision-text").as_str().unwrap().contains("can't meter"));
}

#[test]
fn a_lens_that_can_only_meter_gets_a_meter_point_and_focus_is_left_alone() {
    let mut r = Rig::new();
    r.switch(true);
    // A fixed-focus front camera
    r.event(Event::FocusPointAvailable { available: false });
    r.event(Event::MeterPointAvailable { available: true });
    r.see((0.4, 0.3));
    assert!(r.focus.is_empty() && r.metered.len() == 1 && at(r.metered[0], (0.4, 0.3)));
    assert!(!r.prop("vision-text").as_str().unwrap().contains("can't meter"));
    // The phone's next state says the same: the point isn't sent again
    r.event(Event::FocusPointAvailable { available: false });
    r.event(Event::MeterPointAvailable { available: true });
    r.see_from((0.4, 0.3), 3.0, "phone");
    assert_eq!(r.metered.len(), 1);
    // Another lens forgets the point
    r.event(Event::CameraSwitched { camera: json!({}) });
    r.see((0.4, 0.3));
    assert_eq!(r.metered.len(), 2);
}

#[test]
fn manual_focus_on_the_camera_card_is_left_alone_until_follow_my_face() {
    let mut r = Rig::new();
    // Restored from the saved settings
    r.event(Event::FocusModeChanged { mode: "manual".into() });
    r.switch(true);
    r.see((0.4, 0.3));
    assert!(r.focus.is_empty() && r.prop("held-note").as_str().unwrap().contains("Manual"));
    assert_eq!(r.prop("held"), true);
    r.ui("follow-clicked", Value::Null);
    r.see((0.4, 0.3));
    assert!(r.focus.len() == 1 && at(r.focus[0], (0.4, 0.3)));
    // The Camera card followed the point, and someone set Manual again by preset
    r.event(Event::FocusModeChanged { mode: "point".into() });
    r.event(Event::FocusModeChanged { mode: "manual".into() });
    r.focus.clear();
    r.plugin.meter.reset();
    r.see((0.6, 0.3));
    // Follow my face still stands until switched off and on
    assert!(r.focus.len() == 1 && at(r.focus[0], (0.6, 0.3)));
}

#[test]
fn a_point_of_ours_coming_back_round_the_bus_isnt_one_picked_by_hand() {
    let mut r = Rig::new();
    r.switch(true);
    r.see((0.4, 0.3));
    assert_eq!(r.focus.len(), 1);
    assert_eq!(r.diagnosis(), "following");
    assert_eq!(r.prop("held"), false);
    // One that isn't ours still counts, however close
    r.event(Event::FocusPointPicked { x: r.focus[0].0 as f32, y: r.focus[0].1 as f32 });
    assert_eq!(r.diagnosis(), "point picked by hand");
}

#[test]
fn it_goes_through_off_on_a_device_change_so_a_point_picked_by_hand_is_forgotten() {
    let mut r = Rig::new();
    r.switch(true);
    r.event(Event::FocusPointPicked { x: 0.9, y: 0.9 });
    assert_eq!(r.diagnosis(), "point picked by hand");
    let calls = |r: &Rig, on: &str| r.host.calls.iter().filter(|c| *c == &format!("want_people Expose for my face {on}")).count();
    let (off, on) = (calls(&r, "false"), calls(&r, "true"));
    r.plugin.set_config(&json!({"on": false})).unwrap();
    r.plugin.set_config(&json!({"on": true})).unwrap();
    r.event(Event::DeviceChanged { source_id: "B".into() });
    assert_eq!((calls(&r, "false"), calls(&r, "true")), (off + 1, on + 1));
    assert_eq!(r.diagnosis(), "following");
}

#[test]
fn the_card_goes_away_for_a_source_that_isnt_a_phone_and_its_key_with_it() {
    let mut r = Rig::new();
    let actions = r.plugin.actions();
    assert_eq!(actions.len(), 1);
    assert_eq!((actions[0].id.as_str(), actions[0].group.as_str()), ("face_exposure.on", "Expose for my face"));
    let run = |r: &mut Rig, params: Value| r.plugin.run_action(&mut r.host, "face_exposure.on", &params);
    assert_eq!(run(&mut r, json!({})).as_deref(), Some("Expose for my face: on"));
    r.event(Event::SourceSelected { source_id: "browser".into() });
    assert_eq!((r.prop("phone-only-hidden"), r.prop("active")), (json!(true), json!(true)));
    assert_eq!(run(&mut r, json!({})), None);
    r.event(Event::SourceSelected { source_id: String::new() });
    assert_eq!(r.prop("phone-only-hidden"), false);
    assert_eq!(run(&mut r, json!({"mode": "hold"})).as_deref(), Some("Expose for my face: off"));
    r.plugin.release_action(&mut r.host, "face_exposure.on", &json!({"mode": "hold"}));
    assert_eq!(r.plugin.get_config(), json!({"on": true}));
    r.event(Event::PhonesChanged { count: 0 });
    assert_eq!(run(&mut r, json!({})), None);
}

#[test]
fn shutting_down_stops_asking_for_people() {
    let mut r = Rig::new();
    r.switch(true);
    r.plugin.shutdown(&mut r.host);
    assert!(!r.wants_people());
}

#[test]
fn it_says_what_it_is_as_python_does() {
    let r = Rig::new();
    let info = r.plugin.info();
    assert_eq!((info.name.as_str(), info.title.as_str(), info.category.as_str(), info.icon.as_str()), ("face_exposure", "Expose for my face", "vision", "sun"));
    assert!(info.optional && info.needs_vision && info.in_presets && info.device_local);
    assert_eq!(r.diagnosis(), "off");
}

fn fixture() -> &'static Value {
    static F: OnceLock<Value> = OnceLock::new();
    F.get_or_init(|| {
        let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/face_exposure.json")).expect("face_exposure.json");
        serde_json::from_str(&text).expect("json")
    })
}

fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("a number, not {v}"))
}

fn pair(v: &Value) -> Option<(f64, f64)> {
    v.as_array().map(|p| (num(&p[0]), num(&p[1])))
}

#[test]
fn the_meter_moves_as_python_does_on_random_walks() {
    for walk in fixture()["meters"].as_array().unwrap() {
        let mut meter = FaceMeter::default();
        for step in walk.as_array().unwrap() {
            let got = meter.update(pair(&step["face"]), num(&step["now"]));
            match (got, pair(&step["point"])) {
                (None, None) => {}
                (Some(g), Some(w)) => assert!(near(g.0, w.0, 1e-12) && near(g.1, w.1, 1e-12), "{step}"),
                (g, _) => panic!("{step} {g:?}"),
            }
        }
    }
}

/// The plugin as a whole sends the same points and says the same things after every step of Python's runs.
#[test]
fn whole_runs_send_what_python_sends() {
    for run in fixture()["runs"].as_array().unwrap() {
        let name = run["name"].as_str().unwrap();
        let mut r = Rig::new();
        r.event(Event::FocusPointAvailable { available: false });
        for (i, op) in run["ops"].as_array().unwrap().iter().enumerate() {
            match op["op"].as_str().unwrap() {
                "switch" => r.switch(op["on"].as_bool().unwrap()),
                "tracked" => {
                    *r.clock.lock().unwrap() += 1.0 / 12.0;
                    let tracks = op["tracks"].as_array().unwrap().clone();
                    let count = tracks.len() as u32;
                    r.event(Event::PeopleTracked { source_id: op["source"].as_str().unwrap().into(), tracks, count });
                }
                "focus_ok" => r.event(Event::FocusPointAvailable { available: op["on"].as_bool().unwrap() }),
                "meter_ok" => r.event(Event::MeterPointAvailable { available: op["on"].as_bool().unwrap() }),
                "hand" => r.event(Event::FocusPointPicked { x: num(&op["x"]) as f32, y: num(&op["y"]) as f32 }),
                "picked" => r.event(Event::FocusModePicked { mode: op["mode"].as_str().unwrap().into() }),
                "mode" => r.event(Event::FocusModeChanged { mode: op["mode"].as_str().unwrap().into() }),
                "follow" => r.ui("follow-clicked", Value::Null),
                "camera" => r.event(Event::CameraSwitched { camera: json!({}) }),
                "stop" => r.plugin.on_stream_stop(&mut r.host),
                "focus" => r.plugin.on_focus_gained(&mut r.host, &StreamInfo { source_id: "phone".into(), url: "url".into(), session: 0 }),
                "view" => {
                    let v = &op["view"];
                    r.host.view = View {
                        zoom: num(&v["zoom"]) as f32, pan_x: num(&v["pan_x"]) as f32, pan_y: num(&v["pan_y"]) as f32,
                        flip_h: v["flip_h"].as_bool().unwrap(), flip_v: v["flip_v"].as_bool().unwrap(), rotation: v["rotation"].as_i64().map(|d| d as i32),
                        stretch_x: num(&op["stretch_x"]) as f32, stretch_y: num(&op["stretch_y"]) as f32, ..View::default()
                    };
                }
                other => panic!("{other}"),
            }
            let mut sent: Vec<(&str, (f64, f64))> = r.focus.drain(..).map(|p| ("focus", p)).collect();
            sent.extend(r.metered.drain(..).map(|p| ("meter", p)));
            if op["op"] == "hand" {
                sent.clear();
            }
            let want = op["sent"].as_array().unwrap();
            assert_eq!(sent.len(), want.len(), "{name} op {i}: {sent:?} {want:?}");
            for (s, w) in sent.iter().zip(want) {
                assert!(s.0 == w[0] && near(s.1 .0, num(&w[1]), 1e-6) && near(s.1 .1, num(&w[2]), 1e-6), "{name} op {i}: {s:?} {w}");
            }
            assert_eq!(r.diagnosis(), op["diagnostics"].as_str().unwrap(), "{name} op {i}");
            if op["op"] != "tracked" && op["op"] != "view" {
                assert_eq!(r.prop("vision-text"), op["status"], "{name} op {i}");
                assert_eq!(r.prop("held"), op["held"], "{name} op {i}");
                if op["held"] == true {
                    assert_eq!(r.prop("held-note"), op["note"], "{name} op {i}");
                }
            }
        }
        assert_eq!(r.plugin.get_config(), run["end"]["config"], "{name}");
        assert_eq!(r.wants_people(), !run["end"]["wanted"].as_array().unwrap().is_empty(), "{name}");
        r.plugin.shutdown(&mut r.host);
        assert!(!r.wants_people(), "{name}");
    }
}
