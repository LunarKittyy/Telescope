// What the footer says about a running stream: a size on its way, frames arriving slowly, and whose fault that is.
mod rig;

use rig::{FakeSource, Listener, Rig};
use serde_json::json;
use slint::ComponentHandle;
use slint_interpreter::Value as SValue;
use std::sync::atomic::Ordering;
use telescope_core::event::Event;
use telescope_core::plugin::Plugin;
use telescope_outputs::stream_output::StreamOutputPlugin;

fn streaming(name: &str) -> Rig {
    let r = Rig::new(name);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    r
}

fn pending(r: &Rig, id: &str) -> Option<(u32, u32)> {
    r.source(id).state.borrow().pending_resolution
}

/// Whether pixel fell behind or caught up, each time it was said.
fn behind_events(r: &Rig) -> Vec<String> {
    r.heard().into_iter().filter_map(|l| l.strip_prefix("panel behind pixel ").map(String::from)).collect()
}

/// How often the phone was asked about its camera rate.
fn asked(r: &Rig) -> u32 {
    r.link.control("pixel").asked.load(Ordering::SeqCst)
}

fn ask_size(r: &Rig, width: u32, height: u32) {
    r.emit(Event::ResolutionChangeRequested { width, height });
}

#[test]
fn resolution_pending_shows_warn_color_until_confirmed() {
    let r = streaming("resolution-warn");
    ask_size(&r, 1280, 720);
    assert_eq!(r.text("fps-tone"), "warn");
    assert_eq!(pending(&r, "pixel"), Some((1280, 720)));
    assert_eq!(r.worker(0).settled.load(Ordering::SeqCst), 1, "the stream settles for the phone's camera reopen");
    r.worker(0).says("fps", "30 fps  1920x1080");
    r.pump();
    assert_eq!(r.text("fps-tone"), "warn", "still the old size");
    assert_eq!(pending(&r, "pixel"), Some((1280, 720)));
    r.worker(0).says("fps", "30 fps  1280x720");
    r.pump();
    assert_eq!(r.text("fps"), "30");
    assert_eq!(r.text("fps-tone"), "");
    assert_eq!(pending(&r, "pixel"), None);
}

#[test]
fn resolution_pending_times_out_to_error_then_self_clears() {
    let r = streaming("resolution-timeout");
    ask_size(&r, 1280, 720);
    r.skip(7900);
    assert_eq!(r.text("fps-tone"), "warn");
    r.skip(200);
    assert_eq!(r.text("fps-tone"), "err");
    assert_eq!(pending(&r, "pixel"), None);
    r.skip(3800);
    assert_eq!(r.text("fps-tone"), "err", "it stays red for four seconds");
    r.skip(300);
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
    assert_eq!(pending(&r, "pixel"), Some((640, 480)));
    r.worker(0).says("fps", "30 fps  1280x720");
    r.pump();
    assert_eq!(r.text("fps-tone"), "warn", "the first size no longer counts");
    r.worker(0).says("fps", "30 fps  640x480");
    r.pump();
    assert_eq!(r.text("fps-tone"), "");
    assert_eq!(pending(&r, "pixel"), None);
}

#[test]
fn resolution_pending_cleared_on_idle_status() {
    let r = streaming("resolution-idle");
    ask_size(&r, 1280, 720);
    r.worker(0).says("idle", "Not streaming");
    r.pump();
    assert_eq!(pending(&r, "pixel"), None);
    assert_eq!(r.text("fps-tone"), "");
    r.skip(9000);
    assert_eq!(r.text("fps-tone"), "", "the timer went with it");
}

#[test]
fn a_resolution_timeout_only_clears_its_own_stream() {
    let r = streaming("resolution-own");
    r.add("oneplus");
    r.pump();
    let focus = |id: &str| {
        r.w.with_flow(|flow, host| flow.focus_stream(host, id));
        r.pump();
    };
    focus("pixel");
    ask_size(&r, 1280, 720);
    r.skip(3000);
    focus("oneplus");
    ask_size(&r, 1920, 1080);
    r.skip(5100); // the first phone's eight seconds are up
    assert_eq!(pending(&r, "pixel"), None);
    assert_eq!(pending(&r, "oneplus"), Some((1920, 1080)));
    assert_eq!(r.w.resolution_timers_running(), 1);
    assert_eq!(r.text("fps-tone"), "warn", "still waiting on its own change");
}

#[test]
fn a_lens_switch_or_new_size_lets_the_stream_settle_before_it_counts_as_behind() {
    let r = streaming("settle");
    r.emit(Event::CameraSwitched { camera: json!({"id": "1"}) });
    ask_size(&r, 1280, 720);
    assert_eq!(r.worker(0).settled.load(Ordering::SeqCst), 2);
    r.stop(None);
    r.pump();
    r.emit(Event::CameraSwitched { camera: json!({"id": "0"}) }); // not streaming: nothing to settle
    r.pump();
    assert_eq!(r.worker(0).settled.load(Ordering::SeqCst), 2);
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
    r.worker(0).says("net", "8 Mbps");
    r.pump();
    assert!(behind_events(&r).is_empty(), "keeping up from the start: nothing to say");
    report_slow(&r);
    r.until("the note", |r| behind_events(r) == ["true"]);
    assert_eq!(r.text("net-tone"), "warn");
    report_slow(&r); // still behind: said once, so a dismissed note stays gone
    assert_eq!(behind_events(&r), ["true"]);
    r.worker(0).says("net", "8 Mbps");
    r.pump();
    assert_eq!(behind_events(&r), ["true", "false"]);
    assert_eq!(r.text("net-tone"), "");
}

#[test]
fn a_camera_making_fewer_frames_is_not_called_a_slow_link() {
    let r = streaming("camera-rate");
    arrives(&r, 24.0, json!({"camera_fps": 24.0}));
    report_slow(&r);
    r.until("the phone's answer", |r| !r.text("fps-tip").is_empty());
    report_slow(&r);
    assert!(r.text("fps-tip").contains("Dim light"));
    assert_eq!(r.text("net-tone"), "");
    assert!(behind_events(&r).is_empty());
    assert_eq!(asked(&r), 1, "the answer holds for a while");
    r.worker(0).says("net", "8 Mbps");
    r.pump();
    assert_eq!(r.text("fps-tip"), "");
    *r.worker(0).arrival.lock().unwrap() = 12.0; // the link got worse too: the camera's rate still holds, but frames go missing now
    report_slow(&r);
    assert_eq!(behind_events(&r), ["true"]);
    assert_eq!(asked(&r), 1);
}

#[test]
fn a_browser_whose_camera_makes_no_more_is_not_behind() {
    for (slow_camera, behind) in [(true, Vec::<String>::new()), (false, vec!["true".to_string()])] {
        let r = Rig::new(if slow_camera { "slow-browser" } else { "behind-browser" });
        r.offer(FakeSource { slow_camera, ..FakeSource::new("browser") });
        r.pick("browser");
        r.start();
        r.worker(0).says("ok", "Streaming");
        r.pump();
        *r.worker(0).arrival.lock().unwrap() = 20.0;
        report_slow(&r);
        let heard: Vec<String> = r.heard().into_iter().filter_map(|l| l.strip_prefix("panel behind browser ").map(String::from)).collect();
        assert_eq!(heard, behind, "camera {slow_camera}");
        assert_eq!(r.text("net-tone"), if slow_camera { "" } else { "warn" });
        assert_eq!(r.text("fps-tip").contains("Dim light"), slow_camera);
    }
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
    *r.worker(0).arrival.lock().unwrap() = 12.0; // no answer at all
    report_slow(&r);
    r.until("the note", |r| behind_events(r) == ["true"]);
    drop(r);
    let r = streaming("rate-zero");
    arrives(&r, 12.0, json!({"camera_fps": 0.0})); // a phone too old to say
    report_slow(&r);
    r.until("the note", |r| behind_events(r) == ["true"]);
}

#[test]
fn a_frame_rate_dynamic_lowered_says_so_instead_of_blaming_the_light() {
    let r = streaming("dynamic-rate");
    arrives(&r, 15.0, json!({"camera_fps": 15.0, "dynamic_fps": 15}));
    report_slow(&r);
    r.until("the tip", |r| !r.text("fps-tip").is_empty());
    report_slow(&r);
    assert_eq!(r.text("fps-tip"), "Dynamic lowered the frame rate to 15 fps to keep up with a slow connection.");
    assert!(behind_events(&r).is_empty());
    assert_eq!(r.text("net-tone"), "");
}

#[test]
fn without_a_dynamic_step_the_camera_is_blamed_as_before() {
    for (i, step) in [json!(0), json!(null), json!("20"), json!(-1)].into_iter().enumerate() { // at the rate asked for, an older phone, junk
        let r = streaming(&format!("no-dynamic-{i}"));
        arrives(&r, 15.0, json!({"camera_fps": 15.0, "dynamic_fps": step}));
        report_slow(&r);
        r.until("the tip", |r| !r.text("fps-tip").is_empty());
        assert!(r.text("fps-tip").contains("Dim light"), "{step}");
        assert_eq!(r.source("pixel").kind.as_phone().unwrap().rates_now().map(|x| x.dynamic), Some(0), "{step}");
    }
}

#[test]
fn a_frame_rate_lowered_for_the_heat_says_so() {
    let r = streaming("heat-rate");
    arrives(&r, 20.0, json!({"camera_fps": 20.0, "dynamic_fps": 24, "heat_fps": 20}));
    report_slow(&r);
    r.until("the tip", |r| !r.text("fps-tip").is_empty());
    report_slow(&r);
    assert_eq!(r.text("fps-tip"), "The phone got hot and lowered the frame rate to 20 fps for now.");
    assert!(behind_events(&r).is_empty());
    assert_eq!(r.text("net-tone"), "");
}

#[test]
fn the_camera_rate_answer_expires() {
    let r = streaming("rate-expires");
    arrives(&r, 24.0, json!({"camera_fps": 24.0}));
    report_slow(&r);
    r.until("the first answer", |r| !r.text("fps-tip").is_empty());
    assert_eq!(asked(&r), 1);
    report_slow(&r);
    assert_eq!(asked(&r), 1, "it holds for a while");
    r.skip(10_100);
    report_slow(&r);
    r.until("the phone asked again", |r| asked(r) == 2);
    r.pump();
    assert_eq!(asked(&r), 2);
}

#[test]
fn a_slow_stream_waits_for_the_phone_and_a_late_answer_after_it_caught_up_says_nothing() {
    let r = streaming("late-answer");
    arrives(&r, 12.0, json!({"camera_fps": 30.0}));
    let control = r.link.control("pixel");
    control.stalled.store(true, Ordering::SeqCst);
    report_slow(&r);
    report_slow(&r);
    assert_eq!(asked(&r), 1, "asked once");
    assert!(behind_events(&r).is_empty(), "it waits for the phone");
    r.worker(0).says("net", "8 Mbps");
    r.pump();
    control.stalled.store(false, Ordering::SeqCst);
    r.until("the answer landed", |r| r.source("pixel").kind.as_phone().unwrap().rates_now().is_some());
    r.pump();
    assert!(behind_events(&r).is_empty());
    report_slow(&r); // the answer is in: no need to ask again
    assert_eq!(behind_events(&r), ["true"]);
    assert_eq!(asked(&r), 1);
}

#[test]
fn a_dropped_stream_is_not_called_slow() {
    let r = streaming("dropped-slow");
    report_slow(&r);
    r.until("the note", |r| behind_events(r) == ["true"]);
    r.worker(0).says("reconnecting", "Reconnecting");
    r.pump();
    assert_eq!(behind_events(&r), ["true", "false"]);
    report_slow(&r); // no frames while it's down
    assert_eq!(behind_events(&r), ["true", "false"]);
    r.worker(0).event(telescope_shell::worker::WorkerEvent::Reconnected);
    r.pump();
    report_slow(&r); // this report still counts the time it was down
    assert_eq!(behind_events(&r), ["true", "false"]);
    report_slow(&r); // really behind now
    r.until("the note again", |r| behind_events(r) == ["true", "false", "true"]);
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
    r.until("the note", |r| behind_events(r) == ["true"]);
    r.worker(0).says("ok", "Streaming"); // the virtual camera reopened, for a new frame rate say
    r.pump();
    assert_eq!(behind_events(&r), ["true", "false"]);
    report_slow(&r);
    r.until("behind again", |r| behind_events(r) == ["true", "false", "true"]);
    r.worker(0).says("reconnecting", "Reconnecting");
    r.pump();
    assert_eq!(behind_events(&r), ["true", "false", "true", "false"]);
    r.worker(0).event(telescope_shell::worker::WorkerEvent::Reconnected);
    r.pump();
    r.forget_heard();
    r.worker(0).says("net", "8 Mbps"); // skipped: it still counts the gap
    r.pump();
    report_slow(&r);
    r.until("behind again", |r| behind_events(r) == ["true"]);
    r.w.with_flow(|flow, host| flow.stop_stream(host, None));
    r.pump();
    assert_eq!(behind_events(&r), ["true", "false"]);
}

#[test]
fn the_cant_keep_up_note_goes_once_the_stream_keeps_up() {
    let r = Rig::with("note-goes", |_, heard| vec![Box::new(StreamOutputPlugin::new(true)) as Box<dyn Plugin>, Box::new(Listener::new("panel", heard))]);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    report_slow(&r);
    let banner = r.banner("behind").expect("a banner");
    assert_eq!(banner["text"], "Try Dynamic or lower quality.", "already on Light");
    assert_eq!(banner["actions"], "Switch to Dynamic");
    r.worker(0).says("net", "8 Mbps");
    r.pump();
    assert!(r.banner("behind").is_none());

    report_slow(&r);
    r.w.ui.invoke("banner-action", &[SValue::String("behind".into()), SValue::Number(0.0)]).unwrap();
    r.pump();
    assert_eq!(r.w.state.borrow().plugin_config("stream_output").unwrap()["bitrate_mbps"], -1);
    assert!(r.banner("behind").is_none());
}

#[test]
fn eliding_label_keeps_its_full_text_available() {
    let r = Rig::new("status-elided");
    r.w.ui.window().set_size(slint::LogicalSize::new(560.0, 600.0));
    r.w.ui.show().unwrap();
    let long = "a considerably longer piece of status text that has no chance of fitting beside the two readouts of a window this narrow";
    let says = |text: &str| i_slint_backend_testing::ElementHandle::find_by_accessible_label(&r.w.ui, text).count();
    let hover = || {
        let position = slint::LogicalPosition::new(40.0, 580.0);
        r.w.ui.window().dispatch_event(slint::platform::WindowEvent::PointerMoved { position });
        r.skip(600);
    };
    r.w.ui.set_property("status", slint_interpreter::Value::String("Short".into())).unwrap();
    hover();
    assert_eq!(says("Short"), 1, "it all shows, so no tooltip");
    r.w.ui.window().dispatch_event(slint::platform::WindowEvent::PointerMoved { position: slint::LogicalPosition::new(300.0, 200.0) });
    r.skip(50);
    r.w.ui.set_property("status", slint_interpreter::Value::String(long.into())).unwrap();
    hover();
    assert_eq!(says(long), 2, "the line and its tooltip");
    let line = i_slint_backend_testing::ElementHandle::find_by_accessible_label(&r.w.ui, long).next().expect("the line");
    assert!(line.size().width < 500.0, "it gives way to the window rather than holding it open: {}", line.size().width);
    assert!(line.absolute_position().x + line.size().width <= 560.0, "and is cut off inside it");
}
