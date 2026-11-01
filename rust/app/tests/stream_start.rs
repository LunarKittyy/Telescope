// A start: the phone wakes first, a stop may overtake it, and the phone's state comes back afterwards.
mod rig;

use rig::{state_with, Listener, Output, Rig};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;
use slint_interpreter::Value as SValue;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::frame::{FrameStep, PixelFormat, ShaderStep};
use telescope_core::host::{Host, HostStreams, OutputChange};
use telescope_core::output::{Held, Outputs, Slot, VideoOutput, WaitScreen};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_outputs::stream_output::StreamOutputPlugin;
use telescope_outputs::wait_screen_plugin::WaitScreenPlugin;
use telescope_shell::shell::Effect;
use telescope_shell::sources::Report;
use telescope_shell::worker::WorkerEvent;

fn on_air(name: &str) -> Rig {
    let r = Rig::new(name);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    r
}

/// Start pressed, and the phone is still waking.
fn waking(name: &str) -> Rig {
    let r = Rig::new(name);
    r.link.hold_wakes(true);
    r.w.toggle_stream();
    r.until("the wake", |r| r.link.logged("wake pixel") == 1);
    r
}

fn stops(r: &Rig, id: &str) -> usize {
    r.link.logged(&format!("stop {id}"))
}

#[test]
fn a_wake_stopped_and_started_again_isnt_stopped_by_the_first_one_landing() {
    let r = waking("wake-again-same-phone");
    r.w.toggle_stream();
    r.until("the stop", |r| stops(r, "pixel") == 1);
    r.w.toggle_stream();
    r.until("the second wake", |r| r.link.logged("wake pixel") == 2);
    r.link.hold_wakes(false);
    r.until("the stream", |r| r.workers_made() == 1);
    r.pump();
    r.skip(100);
    assert_eq!(stops(&r, "pixel"), 1, "the phone is meant to be streaming");
    assert_eq!(r.link.logged("release pixel"), 1, "by the stop alone");
    assert_eq!(r.streaming(), ["pixel"]);
}

#[test]
fn a_late_wake_under_another_phones_only_stops_its_own() {
    let r = waking("wake-late-other-phone");
    r.w.toggle_stream();
    r.until("the stop", |r| stops(r, "pixel") == 1);
    r.link.pair("oneplus", "OnePlus");
    r.pick("oneplus");
    r.w.toggle_stream();
    r.until("the other wake", |r| r.link.logged("wake oneplus") == 1);
    r.link.hold_wakes(false);
    r.until("the first phone stopped for good", |r| stops(r, "pixel") == 2);
    r.until("the other stream", |r| r.workers_made() == 1);
    r.pump();
    assert_eq!(stops(&r, "oneplus"), 0);
    assert_eq!(r.link.logged("release oneplus"), 0);
    assert_eq!(r.streaming(), ["oneplus"]);
}

/// Remembers the main camera's wait screen being shown or let go.
#[derive(Clone, Default)]
struct Screens(Arc<Mutex<Vec<&'static str>>>);

impl Held for Screens {
    fn start(&self) {}
    fn stop(&self) {
        self.0.lock().unwrap().push("stop");
    }
    fn running(&self) -> bool {
        false
    }
}

impl WaitScreen for Screens {
    fn show(&self, _width: u32, _height: u32, _path: Option<&str>, _mirror: bool) {
        self.0.lock().unwrap().push("show");
    }
    fn set_watched(&self, _watched: bool) {}
}

impl Outputs for Screens {
    fn slots(&self) -> Vec<Slot> {
        Vec::new()
    }
    fn formats(&self) -> Vec<PixelFormat> {
        vec![PixelFormat::Rgba]
    }
    fn open(&self, _slot: u32, _width: u32, _height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        Err("no cameras in tests".into())
    }
    fn wait_screen(&self, slot: u32) -> Option<Box<dyn WaitScreen>> {
        (slot == 0).then(|| Box::new(self.clone()) as Box<dyn WaitScreen>)
    }
}

#[test]
fn stopping_the_last_stream_brings_the_wait_screen_back() {
    let screens = Screens::default();
    let shown = screens.0.clone();
    let r = Rig::with("wait-screen-back", move |_, _| vec![Box::new(WaitScreenPlugin::new(Arc::new(screens))) as Box<dyn Plugin>]);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    shown.lock().unwrap().clear();
    r.stop(None);
    r.pump();
    assert_eq!(*shown.lock().unwrap(), ["show"]);
}

fn wake_id(r: &Rig) -> u64 {
    r.source("pixel").kind.as_phone().unwrap().wake_id()
}

#[test]
fn start_builds_worker_pipeline_and_notifies_plugins() {
    let r = Rig::with("start-builds", |_, heard| {
        let output = Output { config: json!({"width": 1280, "height": 720, "fps": 25}) };
        let mut setup = Listener::new("setup", heard);
        setup.config = json!({"canvas_preset": "1080p 16:9 - 1920 x 1080"});
        vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(output), Box::new(setup), Box::new(Stepper)]
    });
    let url = "https://pixel:8080/v1/video.h264";
    r.link.set_token("pixel", "tok");
    *r.link.progress.lock().unwrap() = vec!["waking...".into()];
    r.link.hold_after_progress.store(true, Ordering::SeqCst);
    r.w.toggle_stream();
    r.until("the wake says how it goes, a start that used to look frozen", |r| r.text("status") == "waking...");
    r.link.hold_after_progress.store(false, Ordering::SeqCst);
    r.until("the worker", |r| r.workers_made() == 1);
    let worker = r.worker(0);
    assert_eq!((worker.width, worker.height, worker.fps), (Some(1280), Some(720), 25));
    assert_eq!(worker.canvas, (Some(1920), Some(1080)));
    assert_eq!(*worker.url.lock().unwrap(), url);
    assert_eq!(worker.first_steps, ["stepper"], "steps are in before the first frame");
    assert_eq!(r.link.logged("open pixel with tok"), 1, "the worker and the controls share the session, on the phone's token");
    r.pump();
    assert_eq!(r.heard().iter().filter(|l| l.contains("stream_started")).collect::<Vec<_>>(), [&format!("panel stream_started {url}"), &format!("setup stream_started {url}")]);
    assert_eq!((r.times_heard("panel start pixel"), r.times_heard("setup start pixel")), (1, 1));
    let session = r.source("pixel").session().unwrap();
    assert_eq!((session.id, session.url.as_str()), (1, url));
    assert!(session.worker.is_some());
    session.client.send(&json!({"action": "ping"}));
    assert_eq!(r.link.control("pixel").actions().last().map(|a| a.0.clone()).as_deref(), Some("ping"), "the session's client is the phone's");
    assert_eq!(r.w.workers_running(), 1);
    assert_eq!(r.w.state.borrow().streams.url.as_deref(), Some(url));
    assert_eq!(r.text("start-text"), "Stop streaming");
    r.until("the state is asked for, for this session", |r| r.link.control("pixel").fetched.load(Ordering::SeqCst) == 1);
    assert_eq!(r.link.control("pixel").refreshed.load(Ordering::SeqCst), 0);
}

#[test]
fn start_uses_defaults_without_optional_plugins() {
    let r = Rig::new("start-defaults");
    r.start();
    let worker = r.worker(0);
    assert_eq!((worker.width, worker.height, worker.fps), (None, None, 30));
    assert_eq!(worker.canvas, (None, None));
}

#[test]
fn start_without_connection_or_invalid_stream_is_noop() {
    let bare = Rig::bare("start-no-connection");
    bare.w.toggle_stream();
    bare.pump();
    assert_eq!(bare.workers_made(), 0);
    assert!(!bare.w.is_streaming());
    assert_eq!(bare.text("start-text"), "Start streaming");
    drop(bare);

    let r = Rig::new("start-no-route");
    r.link.no_route.store(true, Ordering::SeqCst);
    r.w.toggle_stream();
    r.pump();
    r.until("the start ended", |r| r.did_hear("panel start_failed"));
    assert_eq!(r.workers_made(), 0);
    assert_eq!(r.heard().iter().filter(|l| *l == "panel start_failed").count(), 1, "the Start that got as far as asking says it ended");
}

#[test]
fn start_wakes_the_phone_before_building_a_worker() {
    let r = waking("start-wakes-first");
    r.pump();
    assert_eq!(r.workers_made(), 0);
    r.link.hold_wakes(false);
    r.until("the worker", |r| r.workers_made() == 1);
}

#[test]
fn start_asks_the_phone_to_open_at_the_size_picked() {
    // After a size too much for the encoder, the phone would otherwise open at it again.
    let r = Rig::with("start-opening", |_, _| vec![Box::new(Output { config: json!({"width": 1920, "height": 1080, "fps": 60}) }) as Box<dyn Plugin>]);
    r.start();
    let openings = r.link.openings.lock().unwrap();
    let (id, opening, after_stop) = openings.last().expect("an opening");
    assert_eq!((id.as_str(), opening.width, opening.height, opening.fps, *after_stop), ("pixel", 1920, 1080, 60, false));
}

#[test]
fn a_wake_right_after_a_stop_tells_the_phone_a_stop_went_first() {
    let r = Rig::with("start-after-stop", |_, _| vec![Box::new(Output { config: json!({"fps": 30}) }) as Box<dyn Plugin>]);
    r.start();
    r.stop(None);
    r.until("the stop got there", |r| stops(r, "pixel") == 1);
    r.start();
    assert!(r.link.openings.lock().unwrap().last().unwrap().2, "the phone may still be winding its stream down");
}

#[test]
fn start_shows_the_reason_and_builds_nothing_when_the_wake_fails() {
    let r = Rig::new("start-wake-fails");
    *r.link.wake_error.lock().unwrap() = Some("Open the app on your phone.".into());
    r.w.toggle_stream();
    r.until("the banner", |r| r.banner("start").is_some());
    let banner = r.banner("start").unwrap();
    assert_eq!(banner["title"], "Couldn't start the phone's camera");
    assert_eq!(banner["text"], "Open the app on your phone.");
    assert_eq!(banner["actions"], "Try again");
    assert_eq!(r.workers_made(), 0);
    assert_eq!(r.times_heard("panel start_failed"), 1);
    assert!(r.flag("start-enabled"), "the button comes back so it can be tried again");
    assert_eq!(r.text("start-text"), "Start streaming");
}

#[test]
fn a_start_problem_banner_clears_on_the_next_start_and_on_a_working_stream() {
    let r = Rig::new("start-banner-clears");
    r.show_issue("start", "Can't connect");
    r.show_issue("vcam", "Resize");
    *r.link.wake_error.lock().unwrap() = Some("still closed".into());
    r.w.toggle_stream();
    r.until("the new reason", |r| r.banner("start").is_some_and(|b| b["text"] == "still closed"));
    *r.link.wake_error.lock().unwrap() = None;
    r.start();
    assert!(r.banner("start").is_none(), "the next start takes it away");
    r.show_issue("start", "Can't connect");
    r.show_issue("vcam", "Resize");
    r.worker(0).says("ok", "Streaming");
    r.pump();
    assert!(r.rows("banners").is_empty(), "{:?}", r.rows("banners"));
}

#[test]
fn a_working_stream_keeps_the_note_on_why_it_fell_back_to_mjpeg() {
    let r = on_air("start-keeps-note");
    r.show_issue("h264", "Switched to Heavy");
    r.worker(0).says("ok", "Streaming");
    r.pump();
    assert!(r.banner("h264").is_some());
}

#[test]
fn a_start_is_not_started_again_while_it_finds_the_phone() {
    let r = waking("start-not-twice");
    r.w.state.borrow_mut().effects.push(Effect::Start { interactive: false }); // an auto-start landing just then
    r.w.process();
    r.pump();
    assert_eq!(r.link.logged("wake pixel"), 1);
    r.link.hold_wakes(false);
    r.until("the stream", |r| r.workers_made() == 1);
}

#[test]
fn a_start_is_not_started_while_the_last_one_still_finds_the_phone() {
    let r = Rig::new("start-while-preparing");
    r.w.streams.flow.borrow_mut().sessions.preparing = true;
    r.w.state.borrow_mut().effects.push(Effect::Start { interactive: false });
    r.w.process();
    r.w.toggle_stream();
    r.pump();
    assert_eq!(r.link.logged("wake pixel"), 0);
}

#[test]
fn a_wake_that_lands_after_the_user_gave_up_is_discarded() {
    let r = waking("wake-gave-up");
    r.w.toggle_stream(); // Stop while the phone is still starting
    r.link.hold_wakes(false);
    r.until("the wake landed, and the phone was stopped again", |r| stops(r, "pixel") == 2);
    r.pump();
    assert_eq!(r.workers_made(), 0);
    assert!(r.streaming().is_empty());
}

#[test]
fn a_wake_that_finishes_after_stop_stops_the_phone_again() {
    let r = waking("wake-after-stop");
    r.w.toggle_stream(); // its stop can reach the phone before the wake's start does
    r.until("the first stop", |r| stops(r, "pixel") == 1);
    r.link.hold_wakes(false);
    r.until("the second stop", |r| stops(r, "pixel") == 2);
}

#[test]
fn a_cancelled_wake_still_stops_a_phone_that_may_be_mid_start() {
    let r = waking("wake-cancelled");
    r.w.toggle_stream();
    r.until("the stop", |r| stops(r, "pixel") == 1);
    r.link.hold_wakes(false);
}

#[test]
fn stop_stream_cancels_a_wake_that_is_still_in_flight() {
    // Re-pairing does this, and it rotates the token the wake is about to hand to a new worker.
    let r = waking("wake-stop-stream");
    r.stop(None);
    r.link.hold_wakes(false);
    r.until("the stops", |r| stops(r, "pixel") == 2);
    r.pump();
    assert_eq!(r.workers_made(), 0);
}

#[test]
fn picking_another_source_while_the_phone_wakes_still_stops_the_phone() {
    let r = waking("wake-picked-away");
    r.offer(rig::FakeSource::new("browser"));
    r.pick("browser"); // Connection moves the pick before asking for the switch
    r.w.with_flow(|flow, host| flow.stop(host, true, None, false));
    r.link.hold_wakes(false);
    r.until("the stops", |r| stops(r, "pixel") == 2);
    r.pump();
    assert_eq!(r.workers_made(), 0);
}

#[test]
fn a_cancelled_phone_that_starts_late_is_stopped_while_another_wakes() {
    let r = Rig::new("wake-late-next-to-another");
    r.link.hold_wakes_of("pixel", true);
    r.link.hold_wakes_of("oneplus", true);
    r.w.toggle_stream();
    r.until("the wake", |r| r.link.logged("wake pixel") == 1);
    *r.link.picked.lock().unwrap() = Some("oneplus".into());
    r.w.state.borrow_mut().switch_device(Some("pixel"), Some("oneplus"));
    r.w.process();
    r.until("the other phone wakes", |r| r.link.logged("wake oneplus") == 1);
    r.until("the first stop", |r| stops(r, "pixel") == 1);
    r.link.hold_wakes_of("pixel", false); // its start went through after the stop got there
    r.until("the second stop", |r| stops(r, "pixel") == 2);
    assert!(r.w.streams.flow.borrow().sessions.waking, "the other phone still wakes");
    r.link.hold_wakes_of("oneplus", false);
    r.until("the other stream", |r| r.streaming() == ["oneplus"]);
}

#[test]
fn a_restart_leaves_a_wake_that_lands_late_alone() {
    let r = waking("wake-restart");
    r.w.with_flow(|flow, host| flow.reconnect_stream(host, None));
    r.link.hold_wakes(false);
    r.until("the stream", |r| r.streaming() == ["pixel"]);
    r.pump();
    assert_eq!(stops(&r, "pixel"), 0, "the phone is meant to keep going");
}

#[test]
fn stop_takes_the_phone_s_camera_down_with_it() {
    let r = on_air("stop-remote");
    r.stop(None);
    r.until("the stop", |r| stops(r, "pixel") == 1);
}

#[test]
fn stop_leaves_an_idle_phone_alone() {
    let r = Rig::new("stop-idle");
    r.w.with_flow(|flow, host| flow.stop(host, true, None, false));
    r.pump();
    assert_eq!(stops(&r, "pixel"), 0);
}

#[test]
fn stop_is_safe_when_already_stopped() {
    let r = Rig::new("stop-twice");
    r.w.set_status("something else", "dim");
    r.w.with_flow(|flow, host| flow.stop(host, true, None, false));
    r.pump();
    assert_eq!(r.text("status"), "Not streaming");
}

#[test]
fn reconnect_and_canvas_reload_leave_the_phone_streaming() {
    let r = on_air("stop-reconnect");
    r.w.with_flow(|flow, host| flow.reconnect_stream(host, None));
    r.until("the new worker", |r| r.workers_made() == 2);
    assert_eq!(stops(&r, "pixel"), 0, "only the desktop side reconnects: the phone's camera is live already");
}

#[test]
fn reconnect_stream_only_restarts_when_active() {
    let r = Rig::new("reconnect-idle");
    r.w.with_flow(|flow, host| flow.reconnect_stream(host, None));
    r.pump();
    assert_eq!(r.link.logged("wake pixel"), 0);
    assert_eq!(stops(&r, "pixel"), 0);
    assert!(!r.did_hear("panel stop") && !r.did_hear("panel stream_stopped"));
    let r = {
        drop(r);
        on_air("reconnect-active")
    };
    r.w.with_flow(|flow, host| flow.reconnect_stream(host, None));
    r.until("the new worker", |r| r.workers_made() == 2);
    assert!(r.worker(1).after_the_others, "the old one stops first");
}

#[test]
fn stop_requests_worker_closes_client_and_notifies_plugins() {
    let r = on_air("stop-tidies");
    r.forget_heard();
    r.w.toggle_stream();
    r.pump();
    assert!(r.worker(0).stops());
    assert!(r.link.control("pixel").closed.load(Ordering::SeqCst));
    assert!(r.source("pixel").session().is_none());
    assert_eq!((r.times_heard("panel stop"), r.times_heard("panel stream_stopped")), (1, 1));
    assert_eq!(r.w.workers_running(), 0);
    assert_eq!(r.text("start-text"), "Start streaming");
}

#[test]
fn toggle_routes_to_start_or_stop() {
    let r = Rig::new("toggle");
    r.start();
    assert_eq!(r.streaming(), ["pixel"]);
    r.add("oneplus");
    r.w.toggle_stream();
    r.pump();
    assert!(r.streaming().is_empty(), "every stream, not just the one on the panels");
}

#[test]
fn is_streaming_and_stop_stream_track_the_session() {
    let r = Rig::new("is-streaming");
    assert!(!r.w.is_streaming());
    r.stop(None); // a guarded no-op while idle
    r.pump();
    assert!(!r.did_hear("panel stream_stopped"));
    r.start();
    assert!(r.w.is_streaming());
    r.stop(None);
    r.pump();
    assert!(!r.w.is_streaming() && r.did_hear("panel stream_stopped"));
}

#[test]
fn drain_phone_stops_is_bounded() {
    // A phone that's gone must not hold the app open at shutdown.
    let r = on_air("drain");
    r.link.stops_hang.store(true, Ordering::SeqCst);
    r.stop(None);
    r.until("the stop went out", |r| stops(r, "pixel") == 1);
    let start = Instant::now();
    r.w.stop_streams_for_quit_by(start + Duration::from_millis(50));
    assert!(start.elapsed() < Duration::from_millis(500), "{:?}", start.elapsed());
    let again = Instant::now();
    r.w.stop_streams_for_quit_by(again + Duration::from_secs(3));
    assert!(again.elapsed() < Duration::from_secs(1), "the stops it gave up on are forgotten");
    r.link.stops_hang.store(false, Ordering::SeqCst);
}

#[test]
fn wake_progress_updates_status_while_waking() {
    let r = waking("wake-progress");
    r.w.phone_report(Report::WakeProgress { source_id: "pixel".into(), wake_id: wake_id(&r), text: "Phone's camera is opening... (3s)".into() });
    r.pump();
    assert_eq!(r.text("status"), "Phone's camera is opening... (3s)");
    r.link.hold_wakes(false);
}

#[test]
fn wake_progress_ignores_a_stale_or_cancelled_wake() {
    let r = waking("wake-progress-stale");
    let id = wake_id(&r);
    let before = r.text("status");
    r.w.phone_report(Report::WakeProgress { source_id: "pixel".into(), wake_id: id - 1, text: "stale tick".into() });
    r.pump();
    assert_eq!(r.text("status"), before);
    r.w.toggle_stream();
    r.pump();
    let before = r.text("status");
    r.w.phone_report(Report::WakeProgress { source_id: "pixel".into(), wake_id: wake_id(&r), text: "too late".into() });
    r.pump();
    assert_eq!(r.text("status"), before);
    r.link.hold_wakes(false);
}

fn phone_states(r: &Rig) -> Vec<String> {
    r.heard().into_iter().filter(|l| l.starts_with("panel state")).collect()
}

fn bus_states(r: &Rig) -> Vec<String> {
    r.heard().into_iter().filter(|l| l.starts_with("panel bus state")).collect()
}

#[test]
fn apply_state_emits_bus_and_calls_plugins() {
    let r = on_air("state-applied");
    let state = state_with(json!({}));
    r.phone_says("pixel", state.clone());
    r.pump();
    assert_eq!(r.w.state.borrow().streams.phone_state, Some(state.clone()));
    assert_eq!(r.times_heard(&format!("panel state {state}")), 1);
    assert_eq!(r.times_heard(&format!("panel bus state {state}")), 1);
}

#[test]
fn apply_state_discards_result_for_inactive_session() {
    let r = on_air("state-stale");
    r.forget_heard();
    let session_id = r.session_id("pixel") + 1;
    r.w.phone_report(Report::State { source_id: "pixel".into(), session_id, state: state_with(json!({})) });
    r.pump();
    assert!(phone_states(&r).is_empty() && bus_states(&r).is_empty());
    assert_eq!(r.w.state.borrow().streams.phone_state, None);
}

#[test]
fn apply_state_rejects_malformed_non_empty_state() {
    let r = on_air("state-malformed");
    r.forget_heard();
    r.phone_says("pixel", json!({"battery": 50})); // missing every other required field
    r.pump();
    assert!(phone_states(&r).is_empty() && bus_states(&r).is_empty());
    assert_eq!(r.w.state.borrow().streams.phone_state, None);
    assert!(r.text("status").contains("can't read"), "{}", r.text("status"));
}

#[test]
fn apply_state_accepts_empty_state() {
    let r = on_air("state-empty");
    r.forget_heard();
    r.phone_says("pixel", json!({}));
    r.pump();
    assert_eq!(phone_states(&r), ["panel state {}"]);
    assert_eq!(bus_states(&r), ["panel bus state {}"]);
    assert_eq!(r.w.state.borrow().streams.phone_state, Some(json!({})));
}

#[test]
fn a_critically_hot_phone_says_the_smaller_size_until_it_cools() {
    let r = on_air("heat-says");
    let hot = |fps: u32| state_with(json!({"heat_width": 1280, "heat_height": 720, "heat_fps": fps}));
    r.phone_says("pixel", hot(15));
    r.pump();
    let banner = r.banner("heat:pixel").expect("a banner");
    assert_eq!(banner["title"], "The phone is very hot");
    assert_eq!(banner["text"], "It's streaming at 1280 × 720 and 15 fps until it cools down.");
    r.phone_says("pixel", hot(0)); // asked for 15 fps or less: only the size dropped
    r.pump();
    assert_eq!(r.banner("heat:pixel").unwrap()["text"], "It's streaming at 1280 × 720 until it cools down.");
    r.phone_says("pixel", state_with(json!({})));
    r.pump();
    assert!(r.banner("heat:pixel").is_none());
}

#[test]
fn heat_that_comes_mid_stream_shows_and_goes_with_the_regular_check() {
    let r = on_air("heat-regular");
    let session_id = r.session_id("pixel");
    let answer = |session_id, state: Value| {
        r.w.phone_report(Report::Heat { source_id: "pixel".into(), session_id, state: Some(state) });
        r.pump();
    };
    answer(session_id, state_with(json!({"heat_width": 1280, "heat_height": 720, "heat_fps": 15})));
    assert_eq!(r.banner("heat:pixel").expect("a banner")["title"], "The phone is very hot");
    answer(session_id + 1, state_with(json!({}))); // an answer for a stream that's gone changes nothing
    assert!(r.banner("heat:pixel").is_some());
    answer(session_id, state_with(json!({})));
    assert!(r.banner("heat:pixel").is_none());
}

#[test]
fn the_heat_note_goes_when_the_stream_stops() {
    let r = on_air("heat-stop");
    r.phone_says("pixel", state_with(json!({"heat_width": 1280, "heat_height": 720})));
    r.pump();
    assert!(r.banner("heat:pixel").is_some());
    r.stop(None);
    r.pump();
    assert!(r.banner("heat:pixel").is_none());
}

#[test]
fn worker_status_updates_status_label() {
    let r = Rig::new("status-label");
    r.start();
    for (kind, tone) in [("ok", "status_dim"), ("warn", "status_warn"), ("other", "status_dim")] {
        r.worker(0).says(kind, "message");
        r.pump();
        assert_eq!(r.text("status"), "message");
        assert_eq!(r.text("status-kind"), tone, "{kind}");
    }
}

#[test]
fn worker_fps_and_idle_status() {
    let r = on_air("status-fps");
    r.worker(0).says("fps", "29.9 fps");
    r.pump();
    assert_eq!(r.text("fps"), "29.9");
    r.worker(0).says("idle", "Not streaming"); // the worker ended by itself: tidied up like a Stop
    r.pump();
    assert_eq!(r.text("fps"), "—");
    assert!(r.source("pixel").session().is_none());
    assert_eq!(r.text("start-text"), "Start streaming");
    assert_eq!(r.times_heard("panel stream_stopped"), 1);
    assert_eq!(r.banner("stopped:pixel").expect("a banner")["title"], "The stream stopped unexpectedly");
}

#[test]
fn a_reconnect_keeps_the_worker_from_hearing_a_gone_stream() {
    let r = on_air("worker-gone");
    let old = r.worker(0);
    r.stop(None);
    r.pump();
    old.event(WorkerEvent::Reconnected); // a worker that's been let go
    old.says("ok", "Streaming");
    r.pump();
    assert_eq!(r.text("status"), "Not streaming");
}

#[test]
fn update_stream_output_forwards_only_provided_values() {
    let r = Rig::new("update-output");
    let change = |change: OutputChange| r.w.update_output(&change, None);
    change(OutputChange { width: Some(Some(1280)), height: Some(Some(720)), fps: None }); // no-op when idle
    r.start();
    change(OutputChange { width: None, height: None, fps: Some(Some(60)) });
    change(OutputChange { width: Some(None), height: Some(None), fps: None }); // None: pass-through
    assert_eq!(*r.worker(0).log.lock().unwrap(), ["update_output None None Some(60)", "update_output Some(None) Some(None) None"]);
}

#[test]
fn falling_behind_on_heavy_offers_light_and_switching_reconnects() {
    let r = Rig::with("heavy-behind", |_, heard| vec![Box::new(StreamOutputPlugin::new(true)) as Box<dyn Plugin>, Box::new(Listener::new("panel", heard))]);
    r.w.state.borrow().file.save(&mut json!({"devices": {"pixel": {"plugin_configs": {"stream_output": {"format": "mjpeg"}}}}}));
    r.w.state.borrow_mut().switch_device(None, Some("pixel"));
    r.w.process();
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    assert!(!r.link.formats.lock().unwrap().last().unwrap().h264);
    assert!(!r.flag("p0-h264"));
    r.worker(0).says("net_warn", "40.0 Mbps");
    r.pump();
    let banner = r.banner("behind").expect("a banner");
    assert_eq!((banner["title"].as_str(), banner["text"].as_str()), ("Can't keep up", "Try Light or lower quality."));
    assert_eq!(banner["actions"], "Switch to Light");
    assert_eq!(banner["kind"], "warn");
    r.w.ui.invoke("banner-action", &[SValue::String("behind".into()), SValue::Number(0.0)]).unwrap();
    r.until("it reconnects", |r| r.workers_made() == 2);
    assert!(r.link.formats.lock().unwrap().last().unwrap().h264);
    assert!(r.flag("p0-h264"), "the card says Light too");
    assert!(r.banner("behind").is_none());
}

/// A plugin with a frame step.
struct Stepper;

impl Plugin for Stepper {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "stepper".into(), title: "Stepper".into(), ..Default::default() }
    }
    fn frame_step(&mut self) -> Option<FrameStep> {
        Some(FrameStep::Shader(ShaderStep { wgsl: String::new(), uniforms: json!({}), textures: Vec::new() }))
    }
}

#[test]
fn packets_problem_says_why_heavy_and_browsers_cant_have_their_packets_changed() {
    use telescope_core::host::HostPackets;
    let r = Rig::with("packets-problem", |link, heard| {
        vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(Output { config: json!({"format": "mjpeg"}) }), Box::new(rig::Picker(link.clone()))]
    });
    let problem = |r: &Rig| r.w.state.borrow_mut().call(0, |_, host| host.packets_problem()).unwrap();
    let picks = |r: &Rig, format: &str| {
        let mut state = r.w.state.borrow_mut();
        let output = state.index("stream_output").unwrap();
        state.slots[output].plugin.as_mut().unwrap().set_config(&json!({"format": format})).unwrap();
    };
    r.w.process();
    assert!(problem(&r).contains("Heavy"));
    picks(&r, "h264");
    assert_eq!(problem(&r), "");
    r.w.state.borrow_mut().streams.url = Some("https://phone/v1/video.mjpeg".into());
    assert!(problem(&r).contains("Heavy"), "what streams counts, not what's picked for next time");
    r.w.state.borrow_mut().streams.url = Some("https://phone/v1/video.udp".into());
    assert_eq!(problem(&r), "");
    r.offer(rig::FakeSource::new("browser"));
    r.pick("browser");
    r.w.process();
    assert_ne!(problem(&r), "");
}

/// Says how it was asked for the camera, and that it isn't ready.
struct Refuses(rig::Log);

impl Plugin for Refuses {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "connection".into(), ..Default::default() }
    }
    fn ensure_virtual_camera(&mut self, _host: &mut dyn Host, interactive: bool, _source_id: Option<&str>) -> bool {
        self.0.lock().unwrap().push(format!("asked interactive {interactive}"));
        false
    }
}

#[test]
fn start_stream_passes_on_that_nobody_asked() {
    let r = Rig::with("start-unasked", |_, heard| vec![Box::new(Refuses(heard.clone())) as Box<dyn Plugin>]);
    r.w.state.borrow_mut().call(0, |_, host| host.start_stream(false));
    r.w.process();
    r.pump();
    assert_eq!(r.heard(), ["asked interactive false"]);
}
