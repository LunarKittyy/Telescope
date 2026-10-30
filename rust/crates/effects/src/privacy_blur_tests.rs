//! test_privacy_blur.py, by name, and the pictures Python recorded (tests/fixtures/make_vision_cards_fixtures.py).

use super::*;
use crate::background::tests::{pattern, recorded, sha, soft_mask, Ctx};
use std::sync::Mutex;
use telescope_core::frame::PixelFormat;
use telescope_core::host::View;
use telescope_core::people::PeopleTracker;
use telescope_core::pose::Person;
use telescope_core::testing::FakeHost;

const EVERY: f64 = 1.0 / 12.0;

fn person(x: f64) -> Person {
    let points = [("nose", x, 0.3), ("left_shoulder", x + 0.05, 0.45), ("right_shoulder", x - 0.05, 0.45), ("left_hip", x + 0.03, 0.8)];
    Person { points: points.iter().map(|(n, px, py)| (n.to_string(), (*px, *py, 0.99))).collect() }
}

#[test]
fn it_blurs_once_a_second_person_is_there_two_updates_in_a_row() {
    let (mut tracker, mut trigger, mut t) = (PeopleTracker::new(), Trigger::default(), 0.0);
    for _ in 0..10 {
        let n = tracker.update(&[person(0.3)], t).len() as u32;
        trigger.update(n, t);
        t += EVERY;
    }
    tracker.update(&[person(0.3), person(0.8)], t);
    // one update could be the model seeing things
    assert!(!trigger.update(tracker.count(), t));
    t += EVERY;
    tracker.update(&[person(0.3), person(0.8)], t);
    assert!(trigger.update(tracker.count(), t));
}

#[test]
fn it_stops_only_once_one_person_has_been_there_for_a_while() {
    let (mut tracker, mut trigger, mut t) = (PeopleTracker::new(), Trigger::default(), 0.0);
    for _ in 0..12 {
        tracker.update(&[person(0.3), person(0.8)], t);
        trigger.update(tracker.count(), t);
        t += EVERY;
    }
    let left = t;
    while t - left < 20.0 {
        tracker.update(&[person(0.3)], t);
        if !trigger.update(tracker.count(), t) {
            break;
        }
        t += EVERY;
    }
    // The tracker lets the second person go after a few seconds, then it waits OFF_S on top
    assert!(t - left >= OFF_S + 2.5);
    assert!(!trigger.on);
}

#[test]
fn a_second_person_flickering_in_and_out_keeps_it_on() {
    let mut trigger = Trigger::default();
    trigger.update(2, 0.0);
    for i in 0..100 {
        // back every 3 s, before OFF_S runs out
        assert!(trigger.update(if i % 30 == 0 { 2 } else { 1 }, i as f64 * 0.1));
    }
}

fn near(a: Box4, b: Box4) -> bool {
    [(a.0, b.0), (a.1, b.1), (a.2, b.2), (a.3, b.3)].iter().all(|(x, y)| (x - y).abs() < 1e-6)
}

#[test]
fn boxes_follow_the_view_and_fade_at_the_edges() {
    let view = |v: View| Geometry::from(&v);
    assert!(near(shown_box((0.4, 0.4, 0.6, 0.6), &view(View { zoom: 2.0, ..View::default() })), (0.3, 0.3, 0.7, 0.7)));
    // people are already found in the mirrored picture
    assert!(near(shown_box((0.1, 0.0, 0.3, 1.0), &view(View { flip_h: true, ..View::default() })), (0.1, 0.0, 0.3, 1.0)));
    // Transforms' Width squeezes them with the picture
    assert!(near(shown_box((0.4, 0.4, 0.6, 0.6), &view(View { stretch_x: 0.5, ..View::default() })), (0.45, 0.4, 0.55, 0.6)));
    let mask = boxes_mask(&[(0.5, 0.0, 1.0, 1.0)], 640, 360);
    let at = |y: usize, x: usize| mask.data[y * mask.w + x];
    assert!(at(10, 5) < 0.05 && at(10, mask.w - 5) > 0.95 && at(10, 20) > 0.2 && at(10, 20) < 0.8);
}

/// Stripes, so a blur shows: the left half and the right half can be checked apart.
fn stripes() -> Rgb {
    let mut f = Rgb::new(640, 360);
    for y in 0..360 {
        for x in (0..640).step_by(4) {
            f.set(x, y, [255, 255, 255]);
        }
    }
    f
}

fn frame_of(rgb: &Rgb) -> Frame {
    let data = rgb.data.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    Frame::new(rgb.w as u32, rgb.h as u32, PixelFormat::Rgba, data)
}

fn sharp(frame: &Rgb, x0: usize, x1: usize) -> bool {
    let mut values = Vec::new();
    for y in 100..260 {
        values.extend_from_slice(&frame.data[(y * frame.w + x0) * 3..(y * frame.w + x1) * 3]);
    }
    let n = values.len() as f64;
    let mean = values.iter().map(|v| *v as f64).sum::<f64>() / n;
    (values.iter().map(|v| (*v as f64 - mean).powi(2)).sum::<f64>() / n).sqrt() > 60.0
}

fn track(id: u64, bbox: Box4, face: (f64, f64), seen_s: f64) -> Track {
    Track { id, bbox, face: Some(face), seen_s, missing_s: 0.0 }
}

struct Rig {
    plugin: PrivacyBlurPlugin,
    host: FakeHost,
    ctx: Ctx,
    clock: Arc<Mutex<f64>>,
    you: Track,
    other: Track,
}

impl Rig {
    fn new() -> Rig {
        let clock = Arc::new(Mutex::new(100.0));
        let reading = clock.clone();
        let mut host = FakeHost::new();
        host.focused = Some("phone".into());
        host.streaming = true;
        host.vision = Some("ready".into());
        let mut rig = Rig {
            plugin: PrivacyBlurPlugin::with_clock(Box::new(move || *reading.lock().unwrap())),
            host,
            ctx: Ctx(None, Default::default()),
            clock,
            you: track(1, (0.1, 0.2, 0.4, 0.9), (0.25, 0.3), 30.0),
            other: track(2, (0.65, 0.3, 0.85, 0.9), (0.75, 0.35), 1.0),
        };
        rig.plugin.setup(&mut rig.host);
        rig.plugin.set_config(&json!({})).unwrap();
        rig.event(Event::PhonesChanged { count: 1 });
        rig
    }

    fn event(&mut self, e: Event) {
        self.plugin.on_event(&mut self.host, &e);
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
    }

    fn switch_on(&mut self) {
        self.ui("switch-toggled-by-hand", vec![json!(true)]);
    }

    fn set_mode(&mut self, mode: &str) {
        let i = MODES.iter().position(|(k, _)| *k == mode).unwrap();
        self.ui("mode-picked", vec![json!(i)]);
    }

    fn see(&mut self, tracks: &[Track], seconds: f64) {
        let records: Vec<Value> = tracks.iter().map(Track::to_value).collect();
        for _ in 0..(seconds * 12.0).round_ties_even() as u32 {
            *self.clock.lock().unwrap() += EVERY;
            self.event(Event::PeopleTracked { source_id: self.host.focused.clone().unwrap(), tracks: records.clone(), count: tracks.len() as u32 });
        }
    }

    fn step(&mut self) -> Box<dyn CpuStep> {
        let Some(FrameStep::Cpu(step)) = self.plugin.frame_step() else { panic!("a CPU step") };
        step
    }

    fn through(&self, step: &mut Box<dyn CpuStep>, rgb: &Rgb) -> Rgb {
        let mut frame = frame_of(rgb);
        step.process(&mut frame, &self.ctx);
        Rgb::from_frame(&frame).unwrap()
    }

    /// process_frame: the frame after the step as it is now.
    fn process(&mut self) -> Rgb {
        let mut step = self.step();
        self.through(&mut step, &stripes())
    }

    fn wanted(&self) -> bool {
        self.host.calls.iter().rev().find_map(|c| c.strip_prefix("want_people Privacy blur ")).is_some_and(|on| on == "true")
    }

    fn shown(&self, key: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
    }

    /// A person mask with these columns set.
    fn mask_of_columns(&mut self, spans: &[(usize, usize)]) {
        let mut mask = Gray::new(640, 360);
        for y in 0..360 {
            for (x0, x1) in spans {
                mask.data[y * 640 + x0..y * 640 + x1].fill(255);
            }
        }
        self.ctx.0 = Some(mask);
    }
}

#[test]
fn switched_on_it_blurs_the_other_person_but_not_you() {
    let mut rig = Rig::new();
    let (you, other) = (rig.you.clone(), rig.other.clone());
    rig.switch_on();
    assert!(rig.wanted());
    rig.see(std::slice::from_ref(&you), 0.5);
    assert!(sharp(&rig.process(), 450, 560));
    rig.see(&[you.clone(), other], 0.5);
    let out = rig.process();
    assert!(sharp(&out, 100, 220) && !sharp(&out, 450, 560));
    rig.see(std::slice::from_ref(&you), OFF_S - 1.0);
    // still on: one person for a little while
    assert!(!sharp(&rig.process(), 450, 560));
    rig.see(&[you], 1.5);
    assert!(sharp(&rig.process(), 450, 560));
}

#[test]
fn all_but_me_blurs_the_room_and_leaves_you() {
    let mut rig = Rig::new();
    let (you, other) = (rig.you.clone(), rig.other.clone());
    rig.switch_on();
    rig.set_mode("background");
    rig.see(&[you, other], 0.5);
    let out = rig.process();
    assert!(sharp(&out, 100, 220) && !sharp(&out, 450, 560) && !sharp(&out, 580, 640));
    // the person mask: you, where your box is
    rig.mask_of_columns(&[(64, 256)]);
    let out = rig.process();
    assert!(sharp(&out, 100, 220) && !sharp(&out, 450, 560));
}

#[test]
fn blur_now_works_without_anyone_found() {
    let mut rig = Rig::new();
    assert_eq!(rig.plugin.get_config()["on"], json!(false));
    assert!(!rig.wanted());
    rig.ui("now-clicked", vec![]);
    let out = rig.process();
    // nobody to tell apart: all of it
    assert!(!sharp(&out, 100, 220) && !sharp(&out, 450, 560));
    rig.ui("now-clicked", vec![]);
    assert!(sharp(&rig.process(), 100, 220));
}

#[test]
fn the_wait_screen_is_asked_for_and_the_picture_blurred_until_it_answers() {
    let mut rig = Rig::new();
    let (you, other) = (rig.you.clone(), rig.other.clone());
    rig.switch_on();
    rig.set_mode("wait");
    rig.see(&[you.clone(), other], 0.5);
    let asks = |host: &FakeHost| host.events.iter().filter_map(|e| if let Event::CoverRequested { on, .. } = e { Some(*on) } else { None }).collect::<Vec<_>>();
    assert_eq!(asks(&rig.host), [true]);
    // nothing answered: Wait screen is off
    assert!(!sharp(&rig.process(), 100, 220));
    assert!(rig.shown("vision-text").as_str().unwrap().contains("Wait screen"));
    rig.event(Event::StreamCovered { covered: true });
    // the wait screen goes over it further on
    assert!(sharp(&rig.process(), 100, 220));
    rig.see(&[you], OFF_S + 1.0);
    assert_eq!(asks(&rig.host), [true, false]);
}

#[test]
fn a_stream_left_blurred_stays_blurred_behind_the_panels() {
    let mut rig = Rig::new();
    let (you, other) = (rig.you.clone(), rig.other.clone());
    rig.switch_on();
    rig.see(&[you.clone(), other], 0.5);
    let mut step = rig.step();
    rig.see(&[you], OFF_S + 1.0);
    assert!(!sharp(&rig.through(&mut step, &stripes()), 450, 560));
}

#[test]
fn a_stream_behind_the_panels_keeps_its_own_view() {
    let mut rig = Rig::new();
    let (you, other) = (rig.you.clone(), rig.other.clone());
    rig.switch_on();
    rig.see(&[you, other], 0.5);
    let mut step = rig.step();
    let before = rig.through(&mut step, &stripes());
    // the panels moved to another phone with its own view
    rig.host.view = View { zoom: 3.0, flip_h: true, ..View::default() };
    rig.event(Event::ViewChanged { view: rig.host.view.clone() });
    assert_eq!(rig.through(&mut step, &stripes()), before);
}

#[test]
fn a_stream_behind_the_panels_blurs_whenever_the_shown_one_does() {
    let mut rig = Rig::new();
    let (you, other) = (rig.you.clone(), rig.other.clone());
    rig.switch_on();
    rig.see(std::slice::from_ref(&you), 0.5);
    // left while it was just you
    let mut behind = rig.step();
    let frame = stripes();
    assert_eq!(rig.through(&mut behind, &frame), frame);
    // someone walks in on the shown camera
    rig.see(&[you.clone(), other], 0.5);
    let out = rig.through(&mut behind, &frame);
    // all of it: nobody to tell apart there
    assert!(!sharp(&out, 100, 220) && !sharp(&out, 450, 560));
    rig.see(&[you], OFF_S + 1.0);
    assert_eq!(rig.through(&mut behind, &frame), frame);
}

#[test]
fn blur_now_with_only_you_found_blurs_all_but_you() {
    let mut rig = Rig::new();
    let you = rig.you.clone();
    rig.switch_on();
    rig.see(&[you], 0.5);
    rig.ui("now-clicked", vec![]);
    let out = rig.process();
    assert!(sharp(&out, 100, 220) && !sharp(&out, 450, 560));
}

#[test]
fn the_person_mask_blurs_all_of_the_other_person_not_just_their_box() {
    let mut rig = Rig::new();
    let (you, other) = (rig.you.clone(), rig.other.clone());
    rig.switch_on();
    rig.see(&[you, other], 0.5);
    // you, and them, wider than the landmarks found
    rig.mask_of_columns(&[(64, 256), (400, 640)]);
    let out = rig.process();
    assert!(sharp(&out, 100, 220) && !sharp(&out, 405, 430) && !sharp(&out, 600, 640));
}

#[test]
fn coming_back_to_a_blurred_stream_keeps_it_blurred() {
    let mut rig = Rig::new();
    let (you, other) = (rig.you.clone(), rig.other.clone());
    let stream = StreamInfo { source_id: "x".into(), url: "url".into(), session: 0 };
    rig.switch_on();
    rig.see(&[you, other], 0.5);
    rig.plugin.on_focus_lost(&mut rig.host);
    rig.host.focused = Some("laptop".into());
    rig.plugin.on_focus_gained(&mut rig.host, &stream);
    // nobody else on this one
    assert!(sharp(&rig.process(), 450, 560));
    rig.plugin.on_focus_lost(&mut rig.host);
    rig.host.focused = Some("phone".into());
    // back, long after the pose model was closed: no update yet
    rig.plugin.on_focus_gained(&mut rig.host, &stream);
    assert!(!sharp(&rig.process(), 450, 560));
}

#[test]
fn a_stream_seen_for_the_first_time_starts_from_what_tracking_knows() {
    let mut rig = Rig::new();
    let (you, other) = (rig.you.clone(), rig.other.clone());
    rig.switch_on();
    rig.host.tracked.insert("laptop".into(), (vec![you.to_value(), other.to_value()], 2));
    rig.plugin.on_focus_lost(&mut rig.host);
    rig.host.focused = Some("laptop".into());
    rig.plugin.on_focus_gained(&mut rig.host, &StreamInfo { source_id: "laptop".into(), url: "url".into(), session: 0 });
    assert!(!sharp(&rig.process(), 450, 560));
}

#[test]
fn you_stay_you_when_your_track_is_lost_and_found_again() {
    let mut rig = Rig::new();
    let (you, other) = (rig.you.clone(), rig.other.clone());
    rig.switch_on();
    rig.see(&[you.clone(), other], 0.5);
    // The model lost both and found them again with new ids; the other one is closer and bigger now
    let you_again = track(7, you.bbox, you.face.unwrap(), 0.5);
    let closer = track(8, (0.55, 0.1, 0.95, 1.0), (0.75, 0.2), 0.5);
    rig.see(&[closer, you_again], 0.5);
    let out = rig.process();
    assert!(sharp(&out, 100, 220) && !sharp(&out, 600, 640));
}

#[test]
fn someone_sitting_right_next_to_you_is_blurred_too() {
    let mut rig = Rig::new();
    let you = rig.you.clone();
    rig.switch_on();
    // inside your grown box
    let beside = track(2, (0.42, 0.2, 0.6, 0.9), (0.51, 0.3), 1.0);
    rig.see(&[you, beside], 0.5);
    // the mask runs the two of you together
    rig.mask_of_columns(&[(64, 390)]);
    let out = rig.process();
    assert!(sharp(&out, 100, 220) && !sharp(&out, 300, 370));
}

#[test]
fn blur_now_asks_for_people_so_it_can_tell_you_apart() {
    let mut rig = Rig::new();
    let (you, other) = (rig.you.clone(), rig.other.clone());
    assert!(!rig.wanted());
    rig.ui("now-clicked", vec![]);
    assert!(rig.wanted());
    rig.see(&[you, other], 0.5);
    let out = rig.process();
    assert!(sharp(&out, 100, 220) && !sharp(&out, 450, 560));
    rig.ui("now-clicked", vec![]);
    assert!(!rig.wanted());
}

#[test]
fn a_visitor_alone_for_a_moment_is_never_taken_for_you() {
    let mut rig = Rig::new();
    let (you, other) = (rig.you.clone(), rig.other.clone());
    rig.switch_on();
    rig.see(&[you.clone(), other.clone()], 0.5);
    // you leaned out long enough for your track to go: just the visitor, once
    rig.see(std::slice::from_ref(&other), EVERY);
    // they're still someone else
    assert!(!sharp(&rig.process(), 450, 560));
    let you_back = track(9, you.bbox, you.face.unwrap(), 0.2);
    rig.see(&[you_back, other], 0.5);
    let out = rig.process();
    assert!(sharp(&out, 100, 220) && !sharp(&out, 450, 560));
}

#[test]
fn while_you_are_away_the_mode_decides_what_gets_blurred() {
    let mut rig = Rig::new();
    let (you, other) = (rig.you.clone(), rig.other.clone());
    rig.switch_on();
    rig.see(&[you, other.clone()], 0.5);
    rig.see(&[other], 0.5);
    let out = rig.process();
    // Blur the others: whoever is there, the room stays
    assert!(sharp(&out, 100, 220) && !sharp(&out, 450, 560));
    rig.mask_of_columns(&[(400, 640)]);
    // all of them, not just their box
    assert!(!sharp(&rig.process(), 600, 640));
    rig.set_mode("background");
    let out = rig.process();
    // Blur all but me, with no you: everything
    assert!(!sharp(&out, 100, 220) && !sharp(&out, 450, 560));
}

// ── Beyond Python's tests ─────────────────────────────────────────────

#[test]
fn keys_do_what_the_card_does() {
    let mut rig = Rig::new();
    let ids: Vec<String> = rig.plugin.actions().iter().map(|a| a.id.clone()).collect();
    assert_eq!(ids, ["privacy_blur.now", "privacy_blur.auto", "privacy_blur.mode"]);
    assert_eq!(rig.plugin.run_action(&mut rig.host, "privacy_blur.now", &json!({})).unwrap(), "Blur now: on");
    assert_eq!(rig.plugin.diagnostics(), vec![("Privacy blur".to_string(), "blurring, others".to_string())]);
    rig.plugin.run_action(&mut rig.host, "privacy_blur.now", &json!({"mode": "off"}));
    rig.plugin.run_action(&mut rig.host, "privacy_blur.auto", &json!({"mode": "on"}));
    assert_eq!(rig.plugin.get_config()["on"], json!(true));
    // saved by the English text
    assert!(rig.plugin.run_action(&mut rig.host, "privacy_blur.mode", &json!({"value": "Blur all but me"})).is_some());
    assert_eq!(rig.plugin.get_config()["mode"], json!("background"));
    // a key held flips it, and letting go puts it back
    rig.plugin.run_action(&mut rig.host, "privacy_blur.now", &json!({"mode": "hold"}));
    assert!(rig.plugin.now);
    rig.plugin.release_action(&mut rig.host, "privacy_blur.now", &json!({"mode": "hold"}));
    assert!(!rig.plugin.now);
    rig.event(Event::PhonesChanged { count: 0 });
    assert!(rig.plugin.run_action(&mut rig.host, "privacy_blur.now", &json!({})).is_none());
}

#[test]
fn settings_round_trip_and_shutdown_lets_go() {
    let mut rig = Rig::new();
    rig.plugin.set_config(&json!({"on": true, "mode": "wait"})).unwrap();
    rig.event(Event::StreamStopped);
    assert_eq!(rig.plugin.get_config(), json!({"on": true, "mode": "wait"}));
    assert!(rig.wanted());
    rig.plugin.set_config(&json!({"on": 1, "mode": "sparkles"})).unwrap();
    assert_eq!(rig.plugin.get_config(), json!({"on": false, "mode": "others"}));
    rig.plugin.set_config(&json!({"on": true})).unwrap();
    rig.ui("now-clicked", vec![]);
    rig.set_mode("wait");
    rig.plugin.shutdown(&mut rig.host);
    assert!(!rig.wanted());
    assert_eq!(rig.host.events.last(), Some(&Event::CoverRequested { owner: "Privacy blur".into(), on: false }));
}

#[test]
fn an_i420_frame_goes_through_too() {
    let mut rig = Rig::new();
    rig.ui("now-clicked", vec![]);
    let mut frame = Frame::new(64, 36, PixelFormat::I420, vec![128; PixelFormat::I420.size(64, 36)]);
    frame.data_mut()[..64 * 36].iter_mut().enumerate().for_each(|(i, v)| *v = if i % 2 == 0 { 40 } else { 200 });
    let mut step = rig.step();
    step.process(&mut frame, &rig.ctx);
    let luma = &frame.data[..64 * 36];
    assert!(luma.iter().all(|v| (*v as i32 - 120).abs() < 12), "{:?}", &luma[..8]);
}

#[test]
fn each_state_makes_the_picture_python_makes() {
    let v = recorded();
    let (w, h) = (v["privacy_size"]["w"].as_u64().unwrap() as usize, v["privacy_size"]["h"].as_u64().unwrap() as usize);
    let tracks = |list: &Value| -> Vec<Track> {
        list.as_array().unwrap().iter().map(|t| {
            let b: Vec<f64> = t["box"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap()).collect();
            track(t["id"].as_u64().unwrap(), (b[0], b[1], b[2], b[3]), (0.5, 0.5), t["seen"].as_f64().unwrap())
        }).collect()
    };
    for case in v["privacy"].as_array().unwrap() {
        let mut rig = Rig::new();
        let z = &case["view"];
        rig.host.view = View {
            zoom: z["zoom"].as_f64().unwrap() as f32, pan_x: z["pan_x"].as_f64().unwrap() as f32, pan_y: z["pan_y"].as_f64().unwrap() as f32,
            flip_h: z["flip_h"] == json!(true), rotation: z["rotation"].as_i64().map(|r| r as i32), stretch_x: z["stretch_x"].as_f64().unwrap() as f32,
            ..View::default()
        };
        rig.ctx.0 = (case["mask"] == json!(true)).then(|| soft_mask(w, h));
        rig.switch_on();
        rig.set_mode(case["mode"].as_str().unwrap());
        if case["now"] == json!(true) {
            rig.ui("now-clicked", vec![]);
        }
        for update in case["updates"].as_array().unwrap() {
            let list = tracks(update);
            rig.see(&list, 0.25);
        }
        let mut step = rig.step();
        let out = rig.through(&mut step, &pattern(w, h));
        assert_eq!(sha(&out.data), case["sha"], "{case}");
    }
}
