// A stream that dropped: the phone is looked for, the stream moves to the route it's on now, and when it won't come back
// the window says why.
mod rig;

use rig::{state_with, Rig};
use serde_json::json;
use std::rc::Rc;
use slint_interpreter::Value as SValue;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use telescope_core::link::{Link, PhoneStatus, Resolution};
use telescope_core::source::RouteSpec;
use telescope_shell::sources::{Report, RECOVER_RETRY};
use telescope_shell::worker::WorkerEvent;

const RETRY: u64 = RECOVER_RETRY.as_millis() as u64 + 50;

const WIFI_URL: &str = "https://192.168.1.20:8080/v1/video.h264";

fn wifi() -> RouteSpec {
    RouteSpec::Network { address: "192.168.1.20".into() }
}

fn ready(route: RouteSpec, streaming: bool) -> Resolution {
    Resolution { route: Some(route), streaming, ..Resolution::new(PhoneStatus::Ready) }
}

fn checks(r: &Rig) -> usize {
    r.link.logged("recovery_check pixel")
}

fn dropped(name: &str, answer: Resolution) -> Rig {
    let r = Rig::new(name);
    r.link.answers_recovery("pixel", answer);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    r.worker(0).says("reconnecting", "Stream dropped - reconnecting");
    r.until("the first probe", |r| checks(r) == 1);
    r.pump();
    r
}

/// Asks again, as the retry timer does, and lets the answer land.
fn probe_again(r: &Rig) {
    let before = checks(r);
    r.skip(RETRY);
    r.until("another probe", |r| checks(r) > before);
    r.pump();
}

fn unreachable(name: &str) -> Rig {
    dropped(name, Resolution::new(PhoneStatus::Unreachable))
}

#[test]
fn a_dropped_stream_asks_the_phone_why_quietly() {
    let r = unreachable("drop-asks");
    assert_eq!(r.times_heard("panel lost pixel"), 1);
    let control = r.link.control("pixel");
    let asked = || control.asked.load(Ordering::SeqCst);
    r.until("the phone was asked", |_| asked() == 1);
    r.pump();
    r.forget_heard();
    probe_again(&r); // got nothing: mid-stream that's no news, not an empty state
    r.until("the next ask", |_| asked() == 2);
    *control.state.lock().unwrap() = Some(state_with(json!({"codec": "h264"})));
    probe_again(&r); // nothing about why it stopped: plugins keep what they have
    r.until("the next ask", |_| asked() == 3);
    r.pump();
    assert!(!r.heard().iter().any(|l| l.starts_with("panel state")), "{:?}", r.heard());
    *control.state.lock().unwrap() = Some(state_with(json!({"codec_error": "Too big"})));
    probe_again(&r);
    r.until("the state came through", |r| r.heard().iter().any(|l| l.starts_with("panel state") && l.contains("Too big")));
    assert_eq!(r.heard().iter().filter(|l| l.starts_with("panel state")).count(), 1);
    assert_eq!(asked(), 4);
    *control.state.lock().unwrap() = None;
    probe_again(&r);
    r.until("the ask is free again", |_| asked() == 5);
}

#[test]
fn a_dropped_stream_keeps_asking_the_phone_why() {
    let r = unreachable("drop-keeps-asking");
    let control = r.link.control("pixel");
    let asked = || control.asked.load(Ordering::SeqCst);
    r.until("the probe asked", |_| asked() == 1);
    probe_again(&r);
    r.until("the next probe asked", |_| asked() == 2);
    r.pump();
    assert_eq!(asked(), 2, "one ask a round");
    r.worker(0).event(telescope_shell::worker::WorkerEvent::Reconnected);
    r.pump();
    let before = checks(&r);
    let asked_now = asked();
    r.skip(RETRY);
    assert_eq!(checks(&r), before, "back: nothing more to ask");
    assert_eq!(asked(), asked_now);
}

#[test]
fn a_camera_another_app_took_is_named_until_the_stream_is_back() {
    let r = unreachable("drop-camera-taken");
    *r.link.control("pixel").state.lock().unwrap() = Some(state_with(json!({"camera_taken": true})));
    probe_again(&r);
    r.until("the banner", |r| r.banner("camera_taken:pixel").is_some());
    assert_eq!(r.banner("camera_taken:pixel").unwrap()["title"], "Another app on the phone is using the camera");
    r.worker(0).event(telescope_shell::worker::WorkerEvent::Reconnected);
    r.pump();
    assert!(r.banner("camera_taken:pixel").is_none());
}

#[test]
fn a_dropped_usb_stream_moves_to_wifi_when_the_cable_is_pulled() {
    let r = Rig::new("drop-moves");
    r.link.answers_recovery("pixel", ready(wifi(), true));
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    let client = r.source("pixel").session().unwrap().client;
    r.worker(0).says("reconnecting", "Stream dropped - reconnecting");
    r.until("the worker moved", |r| r.worker(0).did(&format!("retarget {WIFI_URL}")));
    assert!(r.did_hear("panel lost pixel"));
    assert_eq!(r.link.logged(&format!("adopt pixel {:?}", wifi())), 1);
    let session = r.source("pixel").session().unwrap();
    assert_eq!(session.url, WIFI_URL);
    assert!(Rc::ptr_eq(&session.client, &client), "the same client, so it keeps what it sent");
    assert_eq!(*r.link.control("pixel").retargeted.lock().unwrap(), [WIFI_URL], "once");
    assert!(!r.link.control("pixel").closed.load(Ordering::SeqCst));
    assert!(r.source("pixel").recovering(), "until frames actually come back");
    probe_again(&r); // and it keeps looking meanwhile
}

/// A move that was on its way since generation lands now, on a USB route.
fn usb_move_lands(r: &Rig, generation: u64) -> Arc<AtomicBool> {
    let phone = r.link.phones().into_iter().find(|p| p.id == "pixel").unwrap();
    let route = RouteSpec::Usb { serial: "SER".into() };
    let session = r.link.open_route(&phone, &route, "/v1/video.h264", None).unwrap();
    let released = r.link.released.lock().unwrap().last().unwrap().clone();
    r.w.phone_report(Report::Moved { source_id: "pixel".into(), session_id: r.session_id("pixel"), generation, route, session: session.into() });
    r.pump();
    released
}

#[test]
fn a_move_landing_after_the_stream_came_back_leaves_it_where_it_is() {
    let r = unreachable("move-too-late");
    let generation = r.source("pixel").kind.as_phone().unwrap().recovery_generation();
    r.worker(0).event(WorkerEvent::Reconnected);
    r.pump();
    assert!(!r.source("pixel").recovering());
    let url = r.source("pixel").session().unwrap().url;

    let released = usb_move_lands(&r, generation);
    assert!(!r.worker(0).did("retarget https://usb-SER:8080/v1/video.h264"));
    assert_eq!(r.source("pixel").session().unwrap().url, url);
    let held = RouteSpec::Network { address: "10.0.0.2".into() };
    r.until("the new route let go and the old one held again", |r| released.load(Ordering::SeqCst) && r.link.logged(&format!("adopt pixel {held:?}")) == 1);
}

#[test]
fn a_move_from_an_earlier_recovery_isnt_taken_for_this_ones() {
    let r = unreachable("move-earlier-recovery");
    let generation = r.source("pixel").kind.as_phone().unwrap().recovery_generation();
    r.worker(0).event(WorkerEvent::Reconnected);
    r.pump();
    r.worker(0).says("reconnecting", "Stream dropped - reconnecting");
    r.pump();
    assert!(r.source("pixel").recovering());
    usb_move_lands(&r, generation);
    assert!(!r.worker(0).did("retarget https://usb-SER:8080/v1/video.h264"));

    let current = r.source("pixel").kind.as_phone().unwrap().recovery_generation();
    usb_move_lands(&r, current);
    assert!(r.worker(0).did("retarget https://usb-SER:8080/v1/video.h264"));
}

#[test]
fn recovery_keeps_looking_while_the_phone_is_unreachable() {
    let r = unreachable("drop-keeps-looking");
    assert!(r.source("pixel").recovering());
    probe_again(&r);
    assert_eq!(r.link.log.lock().unwrap().iter().filter(|l| l.starts_with("adopt")).count(), 0);
    let usb = RouteSpec::Usb { serial: "SER".into() };
    r.link.answers_recovery("pixel", ready(usb.clone(), true)); // plugged back in: a fresh forward
    probe_again(&r);
    r.until("the worker moved", |r| r.worker(0).did("retarget https://usb-SER:8080/v1/video.h264"));
    assert_eq!(r.link.logged(&format!("adopt pixel {usb:?}")), 1);
}

#[test]
fn recovery_moves_the_stream_once_per_route() {
    let r = dropped("drop-once", ready(wifi(), true));
    r.until("the worker moved", |r| r.worker(0).did(&format!("retarget {WIFI_URL}")));
    probe_again(&r);
    assert_eq!(r.link.logged(&format!("adopt pixel {:?}", wifi())), 1, "the worker keeps retrying it on its own");
}

#[test]
fn recovery_ends_when_frames_come_back() {
    let r = unreachable("drop-ends");
    r.worker(0).event(telescope_shell::worker::WorkerEvent::Reconnected);
    r.pump();
    assert!(!r.source("pixel").recovering());
    let before = checks(&r);
    r.skip(RETRY);
    assert_eq!(checks(&r), before);
}

#[test]
fn a_probe_from_before_the_stream_came_back_is_dropped() {
    let r = unreachable("drop-late-probe");
    let generation = r.source("pixel").kind.as_phone().unwrap().recovery_generation();
    let session_id = r.session_id("pixel");
    r.worker(0).event(telescope_shell::worker::WorkerEvent::Reconnected);
    r.pump();
    r.w.phone_report(Report::RecoveryProbed { source_id: "pixel".into(), session_id, generation, resolution: ready(wifi(), true) });
    r.pump();
    assert_eq!(r.link.log.lock().unwrap().iter().filter(|l| l.starts_with("adopt")).count(), 0);
    assert!(!r.worker(0).log.lock().unwrap().iter().any(|l| l.starts_with("retarget")));
}

#[test]
fn recovery_stops_the_stream_when_the_phone_stopped_streaming() {
    let r = dropped("drop-phone-stopped", ready(wifi(), false));
    assert!(r.streaming().is_empty());
    assert_eq!(r.link.logged("stop pixel"), 0, "nothing to stop on the phone");
    assert!(!r.source("pixel").recovering());
    assert_eq!(r.banner("stopped:pixel").expect("a banner")["title"], "The phone stopped streaming");
    let workers = r.workers_made();
    let starts = r.link.logged("start_route pixel");
    r.w.ui.invoke("banner-action", &[SValue::String("stopped:pixel".into()), SValue::Number(0.0)]).unwrap();
    r.until("this phone starts", |r| r.workers_made() > workers);
    assert_eq!(r.link.logged("start_route pixel"), starts + 1, "named, not whatever's picked");
    assert_eq!(r.streaming(), ["pixel"]);
}

#[test]
fn recovery_leaves_a_stream_another_computer_started_alone() {
    let r = dropped("drop-other-computer", Resolution { streaming_for: "Office PC".into(), ..ready(wifi(), true) });
    assert!(r.streaming().is_empty());
    assert_eq!(r.link.log.lock().unwrap().iter().filter(|l| l.starts_with("adopt")).count(), 0);
    assert!(!r.worker(0).log.lock().unwrap().iter().any(|l| l.starts_with("retarget")));
    assert_eq!(r.link.logged("stop pixel"), 0);
    assert!(!r.source("pixel").recovering());
    assert_eq!(r.banner("stopped:pixel").expect("a banner")["title"], "The phone is streaming to Office PC");
}

#[test]
fn recovery_stops_and_says_why_when_the_phone_wont_take_the_stream_back() {
    for status in [PhoneStatus::NotPaired, PhoneStatus::LocalOnly, PhoneStatus::PhoneOutdated, PhoneStatus::DesktopOutdated] {
        let r = dropped("drop-wont-take", Resolution::new(status));
        assert_eq!(r.link.logged(&format!("problem pixel {status:?}")), 1);
        assert!(r.streaming().is_empty() && !r.source("pixel").recovering());
        let before = checks(&r);
        r.skip(RETRY);
        assert_eq!(checks(&r), before, "no more looking");
    }
}

#[test]
fn a_stream_that_never_got_its_first_frame_looks_for_the_phone_too() {
    let r = Rig::new("drop-first-frame");
    r.link.answers_recovery("pixel", ready(wifi(), true));
    r.start();
    r.worker(0).says("waiting", "Can't reach the phone's stream. Trying again in 3 s…");
    r.until("the worker moved", |r| r.worker(0).did(&format!("retarget {WIFI_URL}")));
    assert_eq!(r.link.logged(&format!("adopt pixel {:?}", wifi())), 1);
}

#[test]
fn recovery_waits_while_the_phone_is_mid_start() {
    let r = dropped("drop-mid-start", Resolution { busy: true, ..ready(wifi(), false) });
    assert_eq!(r.streaming(), ["pixel"]);
    assert_eq!(r.link.log.lock().unwrap().iter().filter(|l| l.starts_with("adopt")).count(), 0);
    probe_again(&r); // still looking
}

#[test]
fn a_phone_gone_a_while_gets_the_wait_screen_then_a_banner_until_it_is_back() {
    let r = unreachable("gone-banner");
    let worker = r.worker(0);
    r.forget_heard();
    assert!(!worker.is_parked());
    r.skip(9_900);
    assert!(!worker.is_parked());
    r.skip(200);
    assert!(worker.is_parked(), "the wait screen takes the camera, not the last frame");
    assert!(r.idle_outputs().starts_with("[0"), "{}", r.idle_outputs());
    r.skip(19_700);
    assert!(r.banner("stopped:pixel").is_none());
    r.skip(200);
    let banner = r.banner("stopped:pixel").expect("a banner");
    assert_eq!((banner["title"].as_str(), banner["text"].as_str()), ("Can't reach Pixel", "Open Telescope on it."));
    assert!(r.source("pixel").recovering(), "still looking for it");
    probe_again(&r);

    assert_eq!(r.times_heard("panel starting"), 0);
    worker.event(telescope_shell::worker::WorkerEvent::Reconnected);
    r.pump();
    assert!(!worker.is_parked());
    assert_eq!(r.times_heard("panel starting"), 1, "the wait screen lets go first");
    assert!(!r.idle_outputs().starts_with("[0"), "{}", r.idle_outputs());
    assert!(r.banner("stopped:pixel").is_none());
    r.skip(30_000); // its clocks went with it
    assert!(!worker.is_parked());
    assert!(r.banner("stopped:pixel").is_none());
}

#[test]
fn a_phone_back_before_the_wait_screen_keeps_its_camera() {
    let r = unreachable("gone-back-early");
    r.forget_heard();
    r.worker(0).event(telescope_shell::worker::WorkerEvent::Reconnected);
    r.pump();
    r.skip(10_000);
    assert!(!r.worker(0).is_parked());
    assert_eq!(r.times_heard("panel starting"), 0, "the wait screen never had it");
    let source = r.source("pixel");
    r.w.with_flow(|flow, host| flow.park(host, &source)); // a timeout that was already on its way
    assert!(!r.worker(0).is_parked());
}

#[test]
fn stopping_a_phone_that_cant_be_reached_takes_its_banner_away() {
    let r = unreachable("gone-stopped");
    r.skip(30_000);
    assert!(r.banner("stopped:pixel").is_some());
    r.stop(None);
    r.pump();
    assert!(r.banner("stopped:pixel").is_none());
    r.skip(30_000); // late: says nothing about a stream that's over
    assert!(r.banner("stopped:pixel").is_none());
}

#[test]
fn a_tile_follows_its_stream_back_to_live() {
    let r = Rig::new("tile-back");
    r.start();
    r.add("oneplus");
    r.worker(0).says("ok", "Streaming");
    r.worker(1).says("ok", "Streaming");
    r.pump();
    r.worker(1).says("waiting", "Waiting for the first frame…");
    r.pump();
    assert_eq!(r.tile("oneplus").unwrap()["state"], "Reconnecting");
    r.worker(1).says("ok", "Streaming");
    r.worker(1).event(telescope_shell::worker::WorkerEvent::Reconnected);
    r.pump();
    assert_eq!(r.tile("oneplus").unwrap()["state"], "Live");
}

#[test]
fn a_background_stream_says_which_one_dropped_and_came_back() {
    let r = Rig::new("background-drop");
    r.start();
    r.add("oneplus");
    r.worker(1).says("ok", "Streaming");
    r.pump();
    assert_eq!(r.shown().as_deref(), Some("oneplus"));
    r.forget_heard();
    r.worker(0).says("reconnecting", "Stream dropped - reconnecting");
    r.worker(0).says("ok", "Streaming");
    r.pump();
    let heard: Vec<String> = r.heard().into_iter().filter(|l| l.starts_with("panel lost") || l.starts_with("panel connected")).collect();
    assert_eq!(heard, ["panel lost pixel", "panel connected pixel"]);
    assert_eq!(r.shown().as_deref(), Some("oneplus"), "the panels stayed where they were");
}

#[test]
fn a_background_stream_that_reconnects_gets_its_settings_again() {
    let r = Rig::new("background-settings");
    r.start();
    r.add("oneplus");
    r.pump();
    let control = r.link.control("pixel");
    r.worker(0).event(telescope_shell::worker::WorkerEvent::Reconnected);
    r.pump();
    assert_eq!(control.resent.load(Ordering::SeqCst), 1);
    let source = r.source("pixel");
    r.w.with_flow(|flow, host| flow.move_focus(host, &source));
    r.worker(0).event(telescope_shell::worker::WorkerEvent::Reconnected);
    r.pump();
    assert_eq!(control.resent.load(Ordering::SeqCst), 1, "the panels' plugins send them for the stream they show");
}

#[test]
fn stream_reconnected_is_noop_without_an_active_session() {
    let r = Rig::new("reconnected-idle");
    let source = r.source("pixel");
    r.w.with_flow(|flow, host| flow.stream_reconnected(host, &source));
    r.pump();
    assert!(!r.heard().iter().any(|l| l.contains("start")));
}

#[test]
fn stream_reconnected_resends_settings_to_every_plugin() {
    let r = Rig::new("reconnected-plugins");
    r.start();
    r.forget_heard();
    r.worker(0).event(telescope_shell::worker::WorkerEvent::Reconnected);
    r.pump();
    assert_eq!((r.times_heard("panel start pixel"), r.times_heard("microphone start pixel")), (1, 1), "{:?}", r.heard());
    let url = format!("url https://pixel:8080/v1/video.h264 session {}", r.session_id("pixel"));
    assert_eq!((r.times_heard(&format!("panel {url}")), r.times_heard(&format!("microphone {url}"))), (1, 1));
}

#[test]
fn reconnecting_status_is_warn_coloured_and_animates_dots() {
    let r = Rig::new("reconnecting-dots");
    r.start();
    r.worker(0).says("reconnecting", "Stream dropped - reconnecting");
    r.pump();
    assert_eq!(r.text("status"), "Stream dropped - reconnecting.");
    assert_eq!(r.text("status-kind"), "status_warn");
    r.skip(1000);
    assert_eq!(r.text("status"), "Stream dropped - reconnecting..");
    r.skip(1000);
    assert_eq!(r.text("status"), "Stream dropped - reconnecting...");
    r.skip(1000);
    assert_eq!(r.text("status"), "Stream dropped - reconnecting.", "loops back to one dot rather than growing forever");
}

#[test]
fn reconnecting_animation_stops_when_another_status_arrives() {
    let r = Rig::new("reconnecting-stops");
    r.start();
    r.worker(0).says("reconnecting", "Stream dropped - reconnecting");
    r.pump();
    r.worker(0).says("ok", "Stream reconnected");
    r.pump();
    r.skip(1000);
    assert_eq!(r.text("status"), "Stream reconnected", "the timer can't fire and overwrite it");
    assert_eq!(r.text("status-kind"), "status_dim", "all is well: plain text, only trouble gets a colour");
}
