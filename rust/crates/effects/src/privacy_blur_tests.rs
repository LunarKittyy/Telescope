//! Privacy blur: the people count turns it on and off, and what it does then is blur all of the picture or ask for the wait screen.

use super::*;
use crate::background::tests::Ctx;
use std::sync::Mutex;
use telescope_core::frame::PixelFormat;
use telescope_core::testing::FakeHost;

const EVERY: f64 = 1.0 / 12.0;

fn person(x: f64) -> Person {
    let points = [("nose", x, 0.3), ("left_shoulder", x + 0.05, 0.45), ("right_shoulder", x - 0.05, 0.45), ("left_hip", x + 0.03, 0.8)];
    Person { points: points.iter().map(|(n, px, py)| (n.to_string(), (*px, *py, 0.99))).collect() }
}

fn people_at(xs: &[f64]) -> Vec<Value> {
    xs.iter().map(|x| person(*x).to_value()).collect()
}

#[test]
fn it_blurs_on_the_first_update_with_two_people() {
    let mut trigger = Trigger::default();
    assert!(!trigger.update(1, 0.0));
    assert!(trigger.update(2, EVERY));
}

#[test]
fn it_stops_only_after_the_hold_with_one_or_none() {
    let mut trigger = Trigger::default();
    trigger.update(3, 0.0);
    assert!(trigger.update(1, 1.0));
    assert!(trigger.update(0, 1.0 + OFF_S - 0.1));
    assert!(!trigger.update(1, 1.0 + OFF_S));
    // and it takes a fresh two to start again
    assert!(!trigger.update(1, 20.0));
    assert!(trigger.update(2, 21.0));
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

#[test]
fn the_count_is_what_the_model_found_not_the_steadied_one() {
    assert_eq!(found_in(&people_at(&[0.2, 0.7])), 2);
    assert_eq!(found_in(&people_at(&[0.5])), 1);
    assert_eq!(found_in(&[]), 0);
    // someone with no landmarks to make a box from isn't someone
    let blank = Person { points: Default::default() }.to_value();
    assert_eq!(found_in(&[person(0.3).to_value(), blank]), 1);
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

struct Rig {
    plugin: PrivacyBlurPlugin,
    host: FakeHost,
    ctx: Ctx,
    clock: Arc<Mutex<f64>>,
}

impl Rig {
    fn new() -> Rig {
        let clock = Arc::new(Mutex::new(100.0));
        let reading = clock.clone();
        let mut host = FakeHost::new();
        host.focused = Some("phone".into());
        host.streaming = true;
        host.vision = Some("ready".into());
        let mut rig = Rig { plugin: PrivacyBlurPlugin::with_clock(Box::new(move || *reading.lock().unwrap())), host, ctx: Ctx(None, Default::default()), clock };
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

    /// This many people found in each update for this many seconds, at the quick rate.
    fn see(&mut self, people: usize, seconds: f64) {
        let xs: Vec<f64> = (0..people).map(|i| 0.2 + 0.3 * i as f64).collect();
        for _ in 0..(seconds / EVERY).round_ties_even() as u32 {
            *self.clock.lock().unwrap() += EVERY;
            self.event(Event::PeopleSeen { source_id: self.host.focused.clone().unwrap(), people: people_at(&xs) });
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

    /// The frame after the step as it is now.
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

    fn asks(&self) -> Vec<bool> {
        self.host.events.iter().filter_map(|e| if let Event::CoverRequested { on, .. } = e { Some(*on) } else { None }).collect()
    }
}

#[test]
fn the_second_person_blurs_the_whole_picture_at_once_and_it_stops_after_the_hold() {
    let mut rig = Rig::new();
    rig.switch_on();
    assert!(rig.wanted());
    rig.see(1, 0.5);
    assert_eq!(rig.process(), stripes());
    // one update with two people is enough
    rig.see(2, EVERY);
    let out = rig.process();
    assert_eq!(out, blurred(&stripes()));
    assert!(!sharp(&out, 100, 220) && !sharp(&out, 450, 560));
    rig.see(1, OFF_S - 0.5);
    assert_eq!(rig.process(), blurred(&stripes()));
    rig.see(1, 1.0);
    assert_eq!(rig.process(), stripes());
}

#[test]
fn nothing_blurs_with_the_switch_off() {
    let mut rig = Rig::new();
    rig.see(3, 0.5);
    assert_eq!(rig.process(), stripes());
    assert!(!rig.wanted());
}

#[test]
fn blur_now_works_without_anyone_found() {
    let mut rig = Rig::new();
    assert_eq!(rig.plugin.get_config()["on"], json!(false));
    assert!(!rig.wanted());
    rig.ui("now-clicked", vec![]);
    assert!(rig.wanted());
    assert_eq!(rig.process(), blurred(&stripes()));
    rig.ui("now-clicked", vec![]);
    assert!(!rig.wanted());
    assert_eq!(rig.process(), stripes());
}

#[test]
fn the_wait_screen_is_asked_for_and_the_picture_blurred_until_it_answers() {
    let mut rig = Rig::new();
    rig.switch_on();
    rig.set_mode("wait");
    rig.see(2, 0.5);
    assert_eq!(rig.asks(), [true]);
    // nothing answered: Wait screen is off
    assert_eq!(rig.process(), blurred(&stripes()));
    assert!(rig.shown("vision-text").as_str().unwrap().contains("Wait screen"));
    rig.event(Event::StreamCovered { covered: true });
    // the wait screen goes over it further on, so the frame is left alone
    assert_eq!(rig.process(), stripes());
    rig.see(1, OFF_S + 1.0);
    assert_eq!(rig.asks(), [true, false]);
}

#[test]
fn the_blur_mode_never_asks_for_the_wait_screen() {
    let mut rig = Rig::new();
    rig.switch_on();
    rig.see(2, 0.5);
    assert!(rig.asks().is_empty());
}

#[test]
fn a_stream_left_blurred_stays_blurred_behind_the_panels() {
    let mut rig = Rig::new();
    rig.switch_on();
    rig.see(2, 0.5);
    let mut step = rig.step();
    rig.see(1, OFF_S + 1.0);
    assert_eq!(rig.through(&mut step, &stripes()), blurred(&stripes()));
}

#[test]
fn a_stream_behind_the_panels_blurs_whenever_the_shown_one_does() {
    let mut rig = Rig::new();
    rig.switch_on();
    rig.see(1, 0.5);
    // left while it was just you
    let mut behind = rig.step();
    let frame = stripes();
    assert_eq!(rig.through(&mut behind, &frame), frame);
    // someone walks in on the shown camera
    rig.see(2, 0.5);
    assert_eq!(rig.through(&mut behind, &frame), blurred(&frame));
    rig.see(1, OFF_S + 1.0);
    assert_eq!(rig.through(&mut behind, &frame), frame);
}

#[test]
fn coming_back_to_a_blurred_stream_keeps_it_blurred() {
    let mut rig = Rig::new();
    let stream = StreamInfo { source_id: "x".into(), url: "url".into(), session: 0 };
    rig.switch_on();
    rig.see(2, 0.5);
    rig.plugin.on_focus_lost(&mut rig.host);
    rig.host.focused = Some("laptop".into());
    rig.plugin.on_focus_gained(&mut rig.host, &stream);
    // nobody else on this one
    assert_eq!(rig.process(), stripes());
    rig.plugin.on_focus_lost(&mut rig.host);
    rig.host.focused = Some("phone".into());
    // back, long after the pose model was closed: no update yet
    rig.plugin.on_focus_gained(&mut rig.host, &stream);
    assert_eq!(rig.process(), blurred(&stripes()));
}

#[test]
fn a_stream_seen_for_the_first_time_starts_from_what_tracking_knows() {
    let mut rig = Rig::new();
    rig.switch_on();
    rig.host.tracked.insert("laptop".into(), (Vec::new(), 2));
    rig.plugin.on_focus_lost(&mut rig.host);
    rig.host.focused = Some("laptop".into());
    rig.plugin.on_focus_gained(&mut rig.host, &StreamInfo { source_id: "laptop".into(), url: "url".into(), session: 0 });
    assert_eq!(rig.process(), blurred(&stripes()));
}

#[test]
fn people_on_a_stream_that_isnt_shown_are_ignored() {
    let mut rig = Rig::new();
    rig.switch_on();
    rig.event(Event::PeopleSeen { source_id: "laptop".into(), people: people_at(&[0.2, 0.6]) });
    assert_eq!(rig.process(), stripes());
}

#[test]
fn keys_do_what_the_card_does() {
    let mut rig = Rig::new();
    let ids: Vec<String> = rig.plugin.actions().iter().map(|a| a.id.clone()).collect();
    assert_eq!(ids, ["privacy_blur.now", "privacy_blur.auto", "privacy_blur.mode"]);
    assert_eq!(rig.plugin.run_action(&mut rig.host, "privacy_blur.now", &json!({})).unwrap(), "Blur now: on");
    assert_eq!(rig.plugin.diagnostics(), vec![("Privacy blur".to_string(), "blurring, blur".to_string())]);
    rig.plugin.run_action(&mut rig.host, "privacy_blur.now", &json!({"mode": "off"}));
    rig.plugin.run_action(&mut rig.host, "privacy_blur.auto", &json!({"mode": "on"}));
    assert_eq!(rig.plugin.get_config()["on"], json!(true));
    // saved by the English text
    assert!(rig.plugin.run_action(&mut rig.host, "privacy_blur.mode", &json!({"value": "Show the wait screen"})).is_some());
    assert_eq!(rig.plugin.get_config()["mode"], json!("wait"));
    // a key held flips it, and letting go puts it back
    rig.plugin.run_action(&mut rig.host, "privacy_blur.now", &json!({"mode": "hold"}));
    assert!(rig.plugin.now);
    rig.plugin.release_action(&mut rig.host, "privacy_blur.now", &json!({"mode": "hold"}));
    assert!(!rig.plugin.now);
    rig.event(Event::PhonesChanged { count: 0 });
    assert!(rig.plugin.run_action(&mut rig.host, "privacy_blur.now", &json!({})).is_none());
}

#[test]
fn a_key_bound_to_a_mode_that_is_gone_picks_the_whole_picture() {
    for old in ["Blur the others", "Blur all but me"] {
        let mut rig = Rig::new();
        rig.plugin.run_action(&mut rig.host, "privacy_blur.mode", &json!({"op": "set", "option": "Show the wait screen"}));
        assert_eq!(rig.plugin.get_config()["mode"], json!("wait"));
        assert!(rig.plugin.run_action(&mut rig.host, "privacy_blur.mode", &json!({"op": "set", "option": old})).is_some(), "{old}");
        assert_eq!(rig.plugin.get_config()["mode"], json!("blur"), "{old}");
    }
}

#[test]
fn settings_round_trip_and_shutdown_lets_go() {
    let mut rig = Rig::new();
    rig.plugin.set_config(&json!({"on": true, "mode": "wait"})).unwrap();
    rig.event(Event::StreamStopped);
    assert_eq!(rig.plugin.get_config(), json!({"on": true, "mode": "wait"}));
    assert!(rig.wanted());
    rig.plugin.set_config(&json!({"on": true, "mode": "blur"})).unwrap();
    assert_eq!(rig.plugin.get_config(), json!({"on": true, "mode": "blur"}));
    rig.plugin.set_config(&json!({"on": 1, "mode": "sparkles"})).unwrap();
    assert_eq!(rig.plugin.get_config(), json!({"on": false, "mode": "blur"}));
    rig.plugin.set_config(&json!({"on": true, "mode": "wait"})).unwrap();
    rig.ui("now-clicked", vec![]);
    rig.plugin.shutdown(&mut rig.host);
    assert!(!rig.wanted());
    assert_eq!(rig.host.events.last(), Some(&Event::CoverRequested { owner: "Privacy blur".into(), on: false }));
}

#[test]
fn a_saved_mode_that_no_longer_exists_becomes_the_whole_picture() {
    for old in ["others", "background", "", "Blur the others"] {
        let mut rig = Rig::new();
        rig.plugin.set_config(&json!({"on": true, "mode": old})).unwrap();
        assert_eq!(rig.plugin.get_config(), json!({"on": true, "mode": "blur"}), "{old}");
        rig.event(Event::StreamStopped);
        rig.see(2, 0.5);
        assert_eq!(rig.process(), blurred(&stripes()), "{old}");
        assert!(rig.asks().is_empty(), "{old}");
    }
    // a settings file with no mode at all too
    let mut rig = Rig::new();
    rig.plugin.set_config(&json!({"on": true})).unwrap();
    assert_eq!(rig.plugin.get_config()["mode"], json!("blur"));
    // and the ones still there stay
    rig.plugin.set_config(&json!({"mode": "wait"})).unwrap();
    assert_eq!(rig.plugin.get_config()["mode"], json!("wait"));
}

#[test]
fn it_copes_with_vision_being_absent() {
    let mut rig = Rig::new();
    rig.host.vision = None;
    rig.switch_on();
    rig.event(Event::VisionStateChanged { state: "missing".into() });
    // no people ever come, and nothing blurs by itself
    assert_eq!(rig.process(), stripes());
    // by hand still works, and so does the wait screen choice
    rig.ui("now-clicked", vec![]);
    assert_eq!(rig.process(), blurred(&stripes()));
    rig.set_mode("wait");
    assert_eq!(rig.asks(), [true]);
    rig.plugin.shutdown(&mut rig.host);
    assert!(!rig.wanted());
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
