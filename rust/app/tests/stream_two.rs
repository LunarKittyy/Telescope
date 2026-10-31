// Two cameras at once: each has its own virtual camera, the panels show one, and the mic stays with the first.
mod rig;

use rig::{state_with, Listener, Output, Rig};
use serde_json::json;
use slint_interpreter::Value as SValue;
use std::sync::atomic::Ordering;
use telescope_core::host::RequestId;
use telescope_core::plugin::Plugin;
use telescope_outputs::stream_output::StreamOutputPlugin;

/// pixel streams and has the mic, oneplus streams and has the panels.
fn two_streams(name: &str) -> Rig {
    two_of(Rig::new(name))
}

fn two_of(r: Rig) -> Rig {
    r.start();
    r.add("oneplus");
    r.worker(0).says("ok", "Streaming");
    r.worker(1).says("ok", "Streaming");
    r.pump();
    r.forget_heard();
    r
}

/// Rig::new, with settings of its own for each phone.
fn settled(name: &str) -> Rig {
    let r = Rig::new(name);
    r.w.state.borrow().file.save(&mut json!({"devices": {
        "pixel": {"plugin_configs": {"panel": {"zoom": 2}, "microphone": {"gain": 2}}},
        "oneplus": {"plugin_configs": {"panel": {"zoom": 3}, "microphone": {"gain": 3}}},
    }}));
    r.w.state.borrow_mut().switch_device(None, Some("pixel"));
    r.w.process();
    two_of(r)
}

fn zoom(r: &Rig) -> serde_json::Value {
    r.w.state.borrow().plugin_config("panel").unwrap()["zoom"].clone()
}

fn focus(r: &Rig, id: &str) {
    r.w.with_flow(|flow, host| flow.focus_stream(host, id));
    r.pump();
}

/// What plugin heard of streams starting, stopping and the panels moving.
fn hooks(r: &Rig, plugin: &str) -> Vec<String> {
    let of_interest = |l: &&String| ["start ", "focus "].iter().any(|h| l.contains(h)) || l.ends_with(" stop") || l.ends_with(" left");
    r.heard().iter().filter(|l| l.starts_with(&format!("{plugin} "))).filter(of_interest).cloned().collect()
}

fn press_tile_stop(r: &Rig, id: &str) {
    r.w.ui.invoke("tile-stop", &[SValue::String(id.into())]).unwrap();
    r.pump();
}

fn tiles(r: &Rig) -> Vec<String> {
    r.rows("tiles").iter().map(|t| t["id"].clone()).collect()
}

fn slots(r: &Rig) -> Vec<u32> {
    r.workers.lock().unwrap().iter().map(|w| w.camera_slot).collect()
}

#[test]
fn a_second_camera_streams_next_to_the_first_to_a_camera_of_its_own() {
    let r = Rig::new("two-second");
    r.start();
    r.add("oneplus");
    r.pump();
    assert_eq!(slots(&r), [0, 1]);
    assert_eq!(r.streaming(), ["pixel", "oneplus"]);
    assert_eq!(r.shown().as_deref(), Some("oneplus"));
    assert_eq!(tiles(&r), ["pixel", "oneplus"]);
    assert_eq!(r.heard().iter().filter(|l| l.starts_with("microphone start")).collect::<Vec<_>>(), ["microphone start pixel"], "the mic stays with the first phone");
}

#[test]
fn clicking_a_tile_brings_its_settings_back() {
    let r = settled("two-tile-settings");
    assert_eq!(zoom(&r), 3);
    r.w.state.borrow_mut().selected = Some("oneplus".into());
    focus(&r, "pixel");
    assert_eq!(r.shown().as_deref(), Some("pixel"));
    assert_eq!(zoom(&r), 2);
    assert_eq!(r.w.state.borrow().file.load()["devices"]["oneplus"]["plugin_configs"]["panel"]["zoom"], 3);
}

#[test]
fn clicking_a_tile_is_a_focus_change_not_a_stop_and_start() {
    let r = two_streams("two-tile-focus");
    focus(&r, "pixel");
    let panel = hooks(&r, "panel");
    assert_eq!(panel, ["panel left", "panel focus pixel"]);
}

#[test]
fn stopping_the_first_camera_hands_the_mic_to_the_next() {
    let r = two_streams("two-stop-first");
    r.stop(Some("pixel"));
    r.pump();
    assert_eq!(r.streaming(), ["oneplus"]);
    assert_eq!(r.shown().as_deref(), Some("oneplus"));
    assert!(r.rows("tiles").is_empty());
    assert!(r.did_hear("microphone stop") && r.did_hear("microphone focus oneplus"), "{:?}", r.heard());
    assert!(!r.did_hear("panel stream_stopped"));
    r.w.toggle_stream();
    r.pump();
    assert!(r.streaming().is_empty() && r.shown().is_none());
    assert!(r.did_hear("panel stream_stopped"));
}

#[test]
fn stop_stops_every_camera() {
    let r = two_streams("two-stop-all");
    r.w.toggle_stream();
    r.pump();
    assert!(r.streaming().is_empty() && r.worker(0).is_stopped() && r.worker(1).is_stopped());
    assert_eq!(r.heard().iter().filter(|l| *l == "microphone stop").count(), 1);
}

#[test]
fn stopping_the_shown_stream_stops_it_then_moves_the_panels_on() {
    let r = two_streams("two-stop-shown");
    r.stop(Some("oneplus"));
    r.pump();
    let panel = hooks(&r, "panel");
    assert_eq!(panel, ["panel stop", "panel focus pixel"]);
}

#[test]
fn the_panels_dont_hear_their_stream_start_again_when_the_mic_moves_onto_it() {
    let r = two_streams("two-mic-moves");
    r.stop(Some("pixel")); // the mic's stream; the panels show the other, which carries on
    r.pump();
    let panel = hooks(&r, "panel");
    assert!(panel.is_empty(), "{panel:?}");
    let mic = hooks(&r, "microphone");
    assert_eq!(mic, ["microphone stop", "microphone focus oneplus"]);
}

#[test]
fn moving_the_mic_to_another_stream_is_a_focus_change_for_it() {
    let r = two_streams("two-make-main");
    r.w.with_flow(|flow, host| assert!(flow.make_main(host, "oneplus")));
    r.pump();
    let mic = hooks(&r, "microphone");
    assert_eq!(mic, ["microphone left", "microphone focus oneplus"]);
}

#[test]
fn picking_another_source_with_two_streaming_replaces_the_one_shown() {
    let r = two_streams("two-pick");
    r.link.pair("third", "Third");
    r.w.with_flow(|flow, host| assert!(flow.pick_source(host, Some("third"))));
    r.until("the new stream", |r| r.workers_made() == 3);
    r.pump();
    let mut streaming = r.streaming();
    streaming.sort();
    assert_eq!(streaming, ["pixel", "third"]);
    assert!(r.worker(1).is_stopped());
    assert_eq!(r.worker(2).camera_slot, 1, "it takes over that camera");
    assert_eq!(r.shown().as_deref(), Some("third"));
    r.w.with_flow(|flow, host| assert!(flow.pick_source(host, Some("pixel"))));
    assert_eq!(r.shown().as_deref(), Some("pixel"));
}

#[test]
fn stop_keeps_the_mics_settings_with_its_own_phone() {
    let r = settled("two-stop-mic");
    r.w.toggle_stream();
    r.pump();
    let devices = r.w.state.borrow().file.load()["devices"].clone();
    assert_eq!(devices["pixel"]["plugin_configs"]["microphone"]["gain"], 2);
    assert_eq!(devices["oneplus"]["plugin_configs"]["microphone"]["gain"], 3);
}

#[test]
fn back_on_a_tile_its_state_is_asked_for_again() {
    let r = two_streams("two-state-again");
    let control = r.link.control("pixel");
    let asked = control.asked.load(Ordering::SeqCst);
    focus(&r, "pixel");
    r.skip(2_000); // as it is now, not as it was when the stream started
    r.until("the phone was asked", |_| control.asked.load(Ordering::SeqCst) > asked);
}

#[test]
fn the_main_camera_counts_as_free_while_only_an_extra_one_streams() {
    let r = two_streams("two-idle-cameras");
    r.stop(Some("pixel"));
    r.pump();
    assert_eq!(r.idle_outputs(), "[0, 2, 3]");
    r.w.toggle_stream();
    r.pump();
    assert_eq!(r.idle_outputs(), "[1, 2, 3]", "nothing streams: the main camera's wait screen follows the stop itself");
}

#[test]
fn a_starting_tiles_stop_cancels_its_start() {
    let r = two_streams("two-starting-tile");
    r.link.pair("third", "Third");
    r.link.hold_wakes(true);
    r.w.with_flow(|flow, host| flow.add_stream(host, "third"));
    r.until("the wake", |r| r.link.logged("wake third") == 1);
    assert!(tiles(&r).contains(&"third".to_string()));
    assert_eq!(r.tile("third").unwrap()["state"], "Starting");
    press_tile_stop(&r, "third");
    {
        let flow = r.w.streams.flow.borrow();
        assert!(!flow.sessions.waking && flow.sessions.wake_source.is_none());
    }
    assert_eq!(r.streaming().len(), 2);
    assert_ne!(r.shown().as_deref(), Some("third"));
    r.link.hold_wakes(false);
    r.until("the phone was stopped twice", |r| r.link.logged("stop third") == 2);
}

#[test]
fn stop_cancels_a_stream_restarting_next_to_the_shown_one() {
    let r = two_streams("two-stop-restart");
    r.link.hold_wakes(true);
    r.w.with_flow(|flow, host| flow.reconnect_stream(host, Some("pixel")));
    r.until("the restart's wake", |r| r.link.logged("wake pixel") == 2);
    r.w.toggle_stream(); // the Stop button
    {
        let flow = r.w.streams.flow.borrow();
        assert!(flow.sessions.streams.is_empty() && !flow.sessions.waking);
    }
    let workers = r.workers_made();
    r.link.hold_wakes(false);
    r.pump();
    r.pump();
    assert!(!r.w.is_streaming());
    assert_eq!(r.workers_made(), workers);
}

#[test]
fn restarting_the_only_stream_left_keeps_its_virtual_camera() {
    let r = two_streams("two-restart-only");
    r.stop(Some("pixel"));
    r.pump();
    assert_eq!(r.source("oneplus").slot(), 1);
    r.w.with_flow(|flow, host| flow.reconnect_stream(host, None));
    r.until("the restart", |r| r.workers_made() == 3);
    r.pump();
    assert_eq!(r.last_worker().camera_slot, 1);
    r.stop(None);
    r.pump();
    *r.link.picked.lock().unwrap() = Some("oneplus".into());
    r.w.toggle_stream();
    r.until("the next start", |r| r.workers_made() == 4);
    assert_eq!(r.last_worker().camera_slot, 0, "a real stop and start alone goes back to the usual camera");
}

#[test]
fn a_stopped_cameras_start_brings_it_back_next_to_the_others() {
    let r = settled("two-start-again");
    r.stop(Some("oneplus"));
    r.pump();
    assert_eq!(r.streaming(), ["pixel"]);
    r.w.start_source(Some("oneplus".into()));
    r.until("it's back", |r| r.streaming().len() == 2);
    assert_eq!(r.shown().as_deref(), Some("oneplus"));
    assert_eq!(zoom(&r), 3, "on its own settings, not the other phone's");
}

#[test]
fn another_cameras_start_working_keeps_a_stopped_ones_banner() {
    let r = two_streams("two-keeps-banner");
    r.show_issue("stopped:pixel", "Pixel stopped streaming");
    r.worker(1).says("ok", "Streaming");
    r.pump();
    assert!(r.banner("stopped:pixel").is_some());
}

#[test]
fn restarting_a_background_stream_starts_it_with_its_own_settings() {
    let r = Rig::with("two-restart-settings", |_, heard| {
        vec![Box::new(Output { config: json!({}) }) as Box<dyn Plugin>, Box::new(Listener::new("panel", heard))]
    });
    r.w.state.borrow().file.save(&mut json!({"devices": {
        "pixel": {"plugin_configs": {"stream_output": {"fps": 15}}},
        "oneplus": {"plugin_configs": {"stream_output": {"fps": 60}}},
    }}));
    r.w.state.borrow_mut().switch_device(None, Some("pixel"));
    r.w.process();
    r.start();
    r.add("oneplus");
    r.w.with_flow(|flow, host| flow.reconnect_stream(host, Some("pixel")));
    r.until("the restart", |r| r.workers_made() == 3);
    r.pump();
    assert_eq!(r.shown().as_deref(), Some("pixel"));
    assert_eq!(r.last_worker().fps, 15);
    assert_eq!(r.link.openings.lock().unwrap().last().unwrap().1.fps, 15);
}

#[test]
fn a_codec_error_while_another_start_prepares_waits_for_it() {
    let r = Rig::new("two-codec-waits");
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    r.link.pair("third", "Third");
    r.link.hold_wakes(true);
    r.w.with_flow(|flow, host| flow.add_stream(host, "third"));
    r.until("the wake", |r| r.link.logged("wake third") == 1);
    r.phone_says("pixel", state_with(json!({"codec_error": "Encoder stopped", "camera_toggle": true})));
    r.pump();
    assert_eq!(r.shown().as_deref(), Some("third"), "the panels stay on the start, so it opens with its own settings");
    r.link.hold_wakes(false);
    r.until("the stream with the error gets them", |r| r.shown().as_deref() == Some("pixel"));
}

/// Only the main camera is set up, and pixel streams to it.
fn one_camera(name: &str) -> Rig {
    let r = Rig::new(name);
    r.cameras.extras.store(0, Ordering::SeqCst);
    r.link.pair("third", "Third");
    r.start();
    r.w.with_flow(|flow, host| flow.add_stream(host, "oneplus"));
    r.pump();
    r
}

fn answer(r: &Rig, yes: bool) {
    r.w.slots_question().expect("the question").invoke("answered", &[SValue::Bool(yes)]).unwrap();
}

#[test]
fn an_extra_camera_that_isnt_set_up_asks_first() {
    let r = one_camera("extra-asks");
    assert!(r.w.slots_question().is_some());
    assert_eq!(r.workers_made(), 1);
    assert_eq!(r.shown().as_deref(), Some("pixel"));
    answer(&r, false);
    r.pump();
    assert!(r.w.slots_question().is_none());
    assert_eq!(r.workers_made(), 1, "the answer was no");
}

#[test]
fn two_phones_waiting_for_the_extra_cameras_both_start() {
    let r = one_camera("extra-both");
    r.w.with_flow(|flow, host| flow.add_stream(host, "third")); // while that's under way
    r.pump();
    answer(&r, true);
    r.until("all three stream", |r| r.streaming().len() == 3);
    assert_eq!(slots(&r), [0, 1, 2]);
}

#[test]
fn stop_drops_starts_waiting_for_the_extra_cameras() {
    let r = one_camera("extra-stop");
    r.w.with_flow(|flow, host| flow.add_stream(host, "third"));
    r.pump();
    r.w.toggle_stream();
    answer(&r, true);
    r.until("the cameras are there", |r| r.cameras.extras.load(Ordering::SeqCst) == 3);
    r.pump();
    r.pump();
    assert!(!r.w.is_streaming());
    assert_eq!(r.workers_made(), 1);
}

#[test]
fn extra_cameras_ready_carries_on_and_a_failure_says_why() {
    let r = one_camera("extra-fails");
    *r.cameras.fails.lock().unwrap() = Some(("too old".into(), "sudo modprobe".into()));
    answer(&r, true);
    r.until("the banner", |r| r.banner("vcam").is_some());
    assert_eq!(r.banner("vcam").unwrap()["title"], "Couldn't add more virtual cameras");
    assert_eq!(r.workers_made(), 1);
}

#[test]
fn extra_cameras_stay_while_a_stream_goes_to_one() {
    let r = two_streams("extra-stay");
    r.w.remove_extras("panel", RequestId(1));
    r.pump();
    assert!(r.did_hear("panel done false Stop the streams going to them first"), "{:?}", r.heard());
}

fn with_output(name: &str) -> Rig {
    Rig::with(name, |_, heard| vec![Box::new(StreamOutputPlugin::new(true)) as Box<dyn Plugin>, Box::new(Listener::new("panel", heard))])
}

fn too_big() -> serde_json::Value {
    state_with(json!({"codecs": ["mjpeg", "h264"], "codec": "mjpeg", "codec_unsupported": true,
                      "codec_error": "H.264 isn't available at 4096x3072 on this phone", "camera_toggle": true}))
}

#[test]
fn a_background_phone_without_h264_falls_back_to_mjpeg() {
    let r = two_of(with_output("codec-fallback"));
    assert!(r.link.formats.lock().unwrap().last().unwrap().h264);
    let session = r.session_id("pixel");
    r.phone_says("pixel", state_with(json!({"codecs": ["mjpeg"], "camera_toggle": true})));
    r.until("it restarted on the MJPEG route", |r| r.source("pixel").session().is_some_and(|s| s.id != session));
    assert_eq!(r.shown().as_deref(), Some("pixel"));
    assert!(!r.link.formats.lock().unwrap().last().unwrap().h264);
}

#[test]
fn a_background_stream_the_encoder_cant_do_gets_the_panels_and_stops() {
    let r = two_of(with_output("codec-background"));
    r.phone_says("pixel", too_big());
    r.until("it stopped", |r| r.source("pixel").session().is_none());
    assert!(r.banner("encoder").is_some());
}

#[test]
fn h264_too_much_for_the_phone_stops_the_stream_with_a_note_that_stays() {
    let r = with_output("codec-too-much");
    r.start();
    r.pump();
    r.phone_says("pixel", too_big());
    r.until("it stopped", |r| r.streaming().is_empty());
    r.pump();
    assert_eq!(r.banner("encoder").expect("the note stays")["title"], "Too much for the phone's H.264 encoder");
    assert_eq!(r.w.state.borrow().plugin_config("stream_output").unwrap()["format"], "h264", "the choice is left to the person");
}
