// What the footer says about a running stream: a size on its way, frames arriving slowly, and whose fault that is.
mod rig;

use rig::Rig;
use serde_json::json;
use telescope_core::event::Event;

fn streaming(name: &str) -> Rig {
    let r = Rig::new(name);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    r
}

fn ask_size(r: &Rig, width: u32, height: u32) {
    r.emit(Event::ResolutionChangeRequested { width, height });
}

#[test]
fn resolution_pending_shows_warn_color_until_confirmed() {
    let r = streaming("resolution-warn");
    ask_size(&r, 1280, 720);
    assert_eq!(r.text("fps-tone"), "warn");
    assert_eq!(r.worker(0).settled.load(std::sync::atomic::Ordering::SeqCst), 1, "the stream settles for the phone's camera reopen");
    r.worker(0).says("fps", "30 fps  1920x1080");
    r.pump();
    assert_eq!(r.text("fps-tone"), "warn", "still the old size");
    r.worker(0).says("fps", "30 fps  1280x720");
    r.pump();
    assert_eq!(r.text("fps"), "30");
    assert_eq!(r.text("fps-tone"), "");
    assert!(r.source("pixel").state.borrow().pending_resolution.is_none());
}

#[test]
fn resolution_pending_times_out_to_error_then_self_clears() {
    let r = streaming("resolution-timeout");
    ask_size(&r, 1280, 720);
    r.skip(8100);
    assert_eq!(r.text("fps-tone"), "err");
    assert!(r.source("pixel").state.borrow().pending_resolution.is_none());
    r.skip(4100);
    assert_eq!(r.text("fps-tone"), "");
}

#[test]
fn resolution_pending_replaced_by_a_second_request() {
    let r = streaming("resolution-second");
    ask_size(&r, 1280, 720);
    r.skip(6000);
    ask_size(&r, 640, 480);
    r.skip(6000);
    assert_eq!(r.text("fps-tone"), "warn", "the second one has its own eight seconds");
    assert_eq!(r.source("pixel").state.borrow().pending_resolution, Some((640, 480)));
    r.worker(0).says("fps", "30 fps  1280x720");
    r.pump();
    assert_eq!(r.text("fps-tone"), "warn", "the first size no longer counts");
}

#[test]
fn resolution_pending_cleared_on_idle_status() {
    let r = streaming("resolution-idle");
    ask_size(&r, 1280, 720);
    r.w.with_flow(|flow, host| flow.stop_stream(host, None));
    r.pump();
    assert_eq!(r.text("fps-tone"), "");
    r.skip(9000);
    assert_eq!(r.text("fps-tone"), "", "the timer went with it");
}

#[test]
fn a_resolution_timeout_only_clears_its_own_stream() {
    let r = streaming("resolution-own");
    ask_size(&r, 1280, 720);
    r.add("oneplus");
    r.pump();
    r.skip(8100);
    assert_eq!(r.text("fps-tone"), "", "the panels moved on, so the footer has nothing to say about it");
    assert!(r.source("pixel").state.borrow().pending_resolution.is_none());
}

#[test]
fn a_lens_switch_or_new_size_lets_the_stream_settle_before_it_counts_as_behind() {
    let r = streaming("settle");
    r.emit(Event::CameraSwitched { camera: json!({"id": "1"}) });
    ask_size(&r, 1280, 720);
    assert_eq!(r.worker(0).settled.load(std::sync::atomic::Ordering::SeqCst), 2);
}

/// Frames arriving at `arrival` while the phone says its camera makes `camera`.
fn arrives(r: &Rig, arrival: f32, state: serde_json::Value) {
    *r.worker(0).arrival.lock().unwrap() = arrival;
    *r.link.control("pixel").state.lock().unwrap() = Some(state);
}

fn report_slow(r: &Rig) {
    r.worker(0).says("net_warn", "2 Mbps");
    r.pump();
}

#[test]
fn an_amber_throughput_readout_says_the_stream_is_behind_once_until_it_catches_up() {
    let r = streaming("behind-once");
    report_slow(&r);
    r.until("the note", |r| r.did_hear("panel behind pixel true"));
    assert_eq!(r.text("net-tone"), "warn");
    report_slow(&r);
    assert_eq!(r.heard().iter().filter(|l| *l == "panel behind pixel true").count(), 1);
    r.worker(0).says("net", "8 Mbps");
    r.pump();
    assert!(r.did_hear("panel behind pixel false"));
    assert_eq!(r.text("net-tone"), "");
}

#[test]
fn a_camera_making_fewer_frames_is_not_called_a_slow_link() {
    let r = streaming("camera-rate");
    arrives(&r, 24.0, json!({"camera_fps": 24.0}));
    report_slow(&r);
    r.until("the phone's answer", |r| !r.text("fps-tip").is_empty());
    assert!(r.text("fps-tip").contains("Dim light"));
    assert_eq!(r.text("net-tone"), "");
    assert!(!r.did_hear("panel behind pixel true"));
}

#[test]
fn frames_lost_between_camera_and_computer_are_a_slow_link() {
    let r = streaming("link-rate");
    arrives(&r, 12.0, json!({"camera_fps": 30.0}));
    report_slow(&r);
    r.until("the note", |r| r.did_hear("panel behind pixel true"));
    assert_eq!(r.text("net-tone"), "warn");
    assert_eq!(r.text("fps-tip"), "");
}

#[test]
fn without_the_camera_rate_a_slow_stream_is_behind_as_before() {
    let r = streaming("no-rate");
    arrives(&r, 12.0, json!({}));
    report_slow(&r);
    r.until("the note", |r| r.did_hear("panel behind pixel true"));
}

#[test]
fn a_frame_rate_dynamic_lowered_says_so_instead_of_blaming_the_light() {
    let r = streaming("dynamic-rate");
    arrives(&r, 15.0, json!({"camera_fps": 15.0, "dynamic_fps": 15}));
    report_slow(&r);
    r.until("the tip", |r| !r.text("fps-tip").is_empty());
    assert_eq!(r.text("fps-tip"), "Dynamic lowered the frame rate to 15 fps to keep up with a slow connection.");
}

#[test]
fn without_a_dynamic_step_the_camera_is_blamed_as_before() {
    let r = streaming("no-dynamic");
    arrives(&r, 15.0, json!({"camera_fps": 15.0, "dynamic_fps": 0}));
    report_slow(&r);
    r.until("the tip", |r| !r.text("fps-tip").is_empty());
    assert!(r.text("fps-tip").starts_with("The phone's camera is making fewer frames"));
}

#[test]
fn a_frame_rate_lowered_for_the_heat_says_so() {
    let r = streaming("heat-rate");
    arrives(&r, 20.0, json!({"camera_fps": 20.0, "dynamic_fps": 24, "heat_fps": 20}));
    report_slow(&r);
    r.until("the tip", |r| !r.text("fps-tip").is_empty());
    assert_eq!(r.text("fps-tip"), "The phone got hot and lowered the frame rate to 20 fps for now.");
}

#[test]
fn the_camera_rate_answer_expires() {
    let r = streaming("rate-expires");
    arrives(&r, 24.0, json!({"camera_fps": 24.0}));
    report_slow(&r);
    r.until("the first answer", |r| !r.text("fps-tip").is_empty());
    let asked = r.link.control("pixel").asked.load(std::sync::atomic::Ordering::SeqCst);
    report_slow(&r);
    assert_eq!(r.link.control("pixel").asked.load(std::sync::atomic::Ordering::SeqCst), asked, "it holds for a while");
    r.skip(10_100);
    report_slow(&r);
    r.until("the phone asked again", |r| r.link.control("pixel").asked.load(std::sync::atomic::Ordering::SeqCst) > asked);
}

#[test]
fn a_slow_stream_waits_for_the_phone_and_a_late_answer_after_it_caught_up_says_nothing() {
    let r = streaming("late-answer");
    arrives(&r, 12.0, json!({"camera_fps": 30.0}));
    r.link.control("pixel").stalled.store(true, std::sync::atomic::Ordering::SeqCst);
    report_slow(&r);
    assert!(!r.did_hear("panel behind pixel true"), "it waits for the phone");
    r.worker(0).says("net", "8 Mbps");
    r.pump();
    r.link.control("pixel").stalled.store(false, std::sync::atomic::Ordering::SeqCst);
    r.pump();
    assert!(!r.did_hear("panel behind pixel true"));
}

#[test]
fn a_dropped_stream_is_not_called_slow() {
    let r = streaming("dropped-slow");
    r.worker(0).says("reconnecting", "Reconnecting");
    r.pump();
    *r.worker(0).arrival.lock().unwrap() = 12.0;
    report_slow(&r);
    assert!(!r.did_hear("panel behind pixel true"));
}

#[test]
fn a_stream_back_from_a_drop_that_keeps_up_says_nothing() {
    let r = streaming("back-keeps-up");
    r.worker(0).says("reconnecting", "Reconnecting");
    r.pump();
    r.worker(0).event(telescope_shell::worker::WorkerEvent::Reconnected);
    r.pump();
    report_slow(&r); // the first report after it's back still counts the gap
    r.worker(0).says("net", "8 Mbps");
    r.pump();
    assert!(!r.heard().iter().any(|l| l.contains("behind pixel")));
}

#[test]
fn a_stream_that_stops_or_reconnects_is_no_longer_behind() {
    let r = streaming("stops-behind");
    report_slow(&r);
    r.until("the note", |r| r.did_hear("panel behind pixel true"));
    r.worker(0).says("reconnecting", "Reconnecting");
    r.pump();
    assert!(r.did_hear("panel behind pixel false"));
    r.worker(0).event(telescope_shell::worker::WorkerEvent::Reconnected);
    r.pump();
    r.forget_heard();
    r.worker(0).says("net", "8 Mbps"); // skipped: it still counts the gap
    r.pump();
    report_slow(&r);
    r.until("behind again", |r| r.did_hear("panel behind pixel true"));
    r.w.with_flow(|flow, host| flow.stop_stream(host, None));
    r.pump();
    assert!(r.did_hear("panel behind pixel false"));
}

#[test]
fn the_cant_keep_up_note_goes_once_the_stream_keeps_up() {
    let r = streaming("note-goes");
    arrives(&r, 24.0, json!({"camera_fps": 24.0}));
    report_slow(&r);
    r.until("the tip", |r| !r.text("fps-tip").is_empty());
    r.worker(0).says("net", "8 Mbps");
    r.pump();
    assert_eq!(r.text("fps-tip"), "");
}
