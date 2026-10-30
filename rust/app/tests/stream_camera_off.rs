// Streaming the mic alone: the camera goes off and comes back while the stream carries on.
mod rig;

use rig::{state_with, Listener, Rig};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use telescope_core::event::Event;
use telescope_core::plugin::Plugin;
use telescope_shell::sources::Report;

fn with_mic(name: &str, enabled: bool) -> Rig {
    Rig::with(name, move |_, heard| {
        let mut mic = Listener::new("microphone", heard);
        mic.config = json!({"enabled": enabled});
        vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(mic)]
    })
}

fn on_air(name: &str) -> Rig {
    let r = with_mic(name, true);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    r
}

fn sent(r: &Rig) -> Vec<Value> {
    r.link.control("pixel").actions().into_iter().filter(|(a, _)| a == "camera_on").map(|(_, v)| v).collect()
}

fn changes(r: &Rig) -> Vec<String> {
    r.heard().into_iter().filter_map(|l| l.strip_prefix("panel camera_on_changed ").map(String::from)).collect()
}

fn can_turn_off(r: &Rig) -> (bool, String) {
    r.w.state.borrow().streams.can_turn_off.clone()
}

#[test]
fn the_camera_goes_off_and_on_while_the_stream_and_mic_carry_on() {
    let r = on_air("camera-off-on");
    r.w.set_camera_on(false, false);
    r.pump();
    assert_eq!(sent(&r), [json!(0)]);
    assert!(r.worker(0).is_stopped() && r.source("pixel").session().unwrap().worker.is_none());
    assert_eq!(r.streaming(), ["pixel"]);
    assert!(!r.host_says_camera_on());
    assert!(r.did_hear("microphone camera off"));
    assert_eq!(changes(&r), ["false"]);
    assert_eq!(r.text("status"), "Streaming the mic, camera off");
    let control = r.link.control("pixel");
    let asked = control.asked.load(Ordering::SeqCst);
    r.skip(5_000);
    r.until("no video, so the phone is asked it's there", |_| control.asked.load(Ordering::SeqCst) > asked);

    r.w.set_camera_on(true, false);
    r.pump();
    assert_eq!(sent(&r).last(), Some(&json!(1)));
    assert_eq!(r.workers_made(), 2);
    assert!(r.source("pixel").session().unwrap().worker.is_some());
    assert!(r.did_hear("microphone camera on"));
    assert_eq!(changes(&r), ["false", "true"]);
    assert!(r.host_says_camera_on());
    let asked = control.asked.load(Ordering::SeqCst);
    r.skip(5_000);
    assert_eq!(control.asked.load(Ordering::SeqCst), asked, "video says the phone is there now");
}

#[test]
fn the_camera_stays_on_without_the_mic() {
    let r = with_mic("camera-no-mic", false);
    r.start();
    r.pump();
    assert!(!can_turn_off(&r).0);
    r.w.set_camera_on(false, false);
    r.pump();
    assert!(r.host_says_camera_on() && sent(&r).is_empty() && changes(&r).is_empty());
}

#[test]
fn start_mic_only_opens_the_phone_with_its_camera_off() {
    let r = with_mic("camera-mic-only", true);
    r.pump();
    r.w.set_camera_on(false, false);
    r.pump();
    assert_eq!(changes(&r), ["false"]);
    assert_eq!(r.text("start-text"), "Start mic only");
    r.w.toggle_stream();
    r.until("the stream", |r| r.streaming() == ["pixel"]);
    assert!(r.link.openings.lock().unwrap().last().expect("an opening").1.camera_off);
    assert_eq!(r.workers_made(), 0);
    assert_eq!(r.text("start-text"), "Stop streaming");
    r.stop(None);
    r.pump();
    assert!(r.host_says_camera_on());
    assert_eq!(changes(&r), ["false", "true"], "the next stream starts with the camera on");
    assert_eq!(r.text("start-text"), "Start streaming");
}

#[test]
fn a_restart_keeps_the_camera_off() {
    let r = on_air("camera-restart");
    r.w.set_camera_on(false, false);
    r.pump();
    r.w.with_flow(|flow, host| flow.reconnect_stream(host, None));
    r.until("the stream's back", |r| r.streaming() == ["pixel"]);
    r.pump();
    assert!(!r.host_says_camera_on());
    assert_eq!(r.workers_made(), 1, "no video worker for the restarted stream either");
}

#[test]
fn a_phone_left_with_its_camera_on_or_off_is_matched() {
    let r = on_air("camera-matched");
    r.phone_says("pixel", state_with(json!({"camera_toggle": true, "camera_off": true})));
    r.pump();
    assert_eq!(sent(&r), [json!(1)]);
}

#[test]
fn an_older_phone_that_cant_turn_its_camera_off_gets_the_camera_back() {
    let r = with_mic("camera-older", true);
    r.w.set_camera_on(false, false);
    r.pump();
    r.w.toggle_stream();
    r.until("the stream", |r| r.streaming() == ["pixel"]);
    r.phone_says("pixel", state_with(json!({}))); // no camera_toggle: it opened its camera anyway
    r.pump();
    assert!(r.host_says_camera_on());
    assert_eq!(r.workers_made(), 1);
    assert_eq!(changes(&r), ["false", "true"]);
    assert!(r.banner("camera").is_some());
    assert!(!can_turn_off(&r).0);
}

#[test]
fn a_camera_that_wont_come_back_on_stays_off_and_says_why() {
    let r = on_air("camera-wont-return");
    r.w.set_camera_on(false, false);
    r.pump();
    r.w.set_camera_on(true, false);
    r.pump();
    let session_id = r.session_id("pixel");
    r.w.phone_report(Report::CameraCheck { source_id: "pixel".into(), session_id,
                                           state: Some(json!({"camera_toggle": true, "camera_off": true, "camera_error": "Another app may be using it."})) });
    r.pump();
    assert!(!r.host_says_camera_on());
    assert!(r.source("pixel").session().unwrap().worker.is_none() && r.worker(1).is_stopped());
    assert_eq!(changes(&r), ["false", "true", "false"]);
    assert!(r.banner("camera").is_some());
}

#[test]
fn switching_the_mic_off_brings_the_camera_back() {
    let r = on_air("camera-mic-gone");
    r.w.set_camera_on(false, false);
    r.pump();
    r.w.state.borrow_mut().apply_device_profile(Some("pixel"), &json!({"devices": {"pixel": {"plugin_configs": {"microphone": {"enabled": false}}}}}));
    r.emit(Event::MicChanged { enabled: false, muted: false });
    r.pump();
    assert!(r.host_says_camera_on());
    assert_eq!(r.workers_made(), 2);
}

#[test]
fn with_the_camera_off_a_phone_that_stops_answering_is_looked_for() {
    let r = on_air("camera-alive");
    r.w.set_camera_on(false, false);
    r.pump();
    let session_id = r.session_id("pixel");
    let alive = |ok| r.w.phone_report(Report::Alive { source_id: "pixel".into(), session_id, ok });
    alive(false);
    assert!(!r.source("pixel").recovering(), "one miss can be a blip");
    alive(false);
    r.pump();
    assert!(r.source("pixel").recovering());
    assert!(r.did_hear("panel lost pixel"));
    alive(true);
    r.pump();
    assert!(!r.source("pixel").recovering());
    assert_eq!(r.text("status"), "Streaming the mic, camera off");
}

#[test]
fn with_two_cameras_neither_turns_its_camera_off() {
    let r = on_air("camera-two");
    r.add("oneplus");
    r.pump();
    let (ok, why) = can_turn_off(&r);
    assert!(!ok && why.contains("one camera"), "{why}");
}

#[test]
fn reconnecting_the_mics_stream_keeps_the_mic_on_it() {
    let r = on_air("camera-mic-stays");
    r.add("oneplus");
    r.pump();
    let source = r.source("pixel");
    r.w.with_flow(|flow, host| flow.move_focus(host, &source));
    r.forget_heard();
    r.w.with_flow(|flow, host| flow.reconnect_stream(host, Some("pixel")));
    r.until("both stream again", |r| r.streaming().len() == 2);
    r.pump();
    assert_eq!(r.w.streams.flow.borrow().sessions.main.as_ref().map(|m| m.id_str().to_string()).as_deref(), Some("pixel"));
    assert_eq!(r.heard().iter().filter(|l| *l == "microphone start pixel").count(), 1, "{:?}", r.heard());
}
