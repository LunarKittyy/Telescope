// Monitoring and Stream output in the real window: what the phone says arrives through the stream's own fetch and a banner's
// button really starts the phone (test_plugins.py, whose stand-in host checks the same things).
mod rig;

use rig::{state_with, Listener, Rig};
use serde_json::{json, Value};
use slint_interpreter::Value as SValue;
use std::sync::atomic::Ordering;
use telescope_core::plugin::Plugin;
use telescope_outputs::stream_output::StreamOutputPlugin;
use telescope_setup::monitoring::{MonitoringPlugin, POLL_MS};
use telescope_shell::sources::Report;

fn with(name: &str, plugin: Box<dyn Plugin>) -> Rig {
    Rig::with(name, move |_, heard| vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, plugin])
}

fn watched(name: &str) -> Rig {
    with(name, Box::new(MonitoringPlugin::new()))
}

fn on_air(r: &Rig) {
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
}

/// What the Phone card shows.
fn battery(r: &Rig) -> String {
    let mut state = r.w.state.borrow_mut();
    let at = state.index("monitoring").unwrap();
    let card = state.slots[at].plugin.as_mut().unwrap().card().unwrap();
    card.ui.props["battery-value"].as_str().unwrap().to_string()
}

fn notes(r: &Rig) -> usize {
    r.notes.0.lock().unwrap().len()
}

/// A phone's whole state, with H.264 on offer so a stream behind the panels isn't called up over its codec.
fn reads(level: u32, charging: bool, temp: f64) -> Value {
    state_with(json!({"codecs": ["mjpeg", "h264"], "battery": level, "charging": charging, "battery_temp_c": temp}))
}

fn press_first_button(r: &Rig, key: &str) {
    r.w.ui.invoke("banner-action", &[SValue::String(key.into()), SValue::Number(0.0)]).unwrap();
}

fn starts(r: &Rig, id: &str) -> usize {
    r.link.logged(&format!("start_route {id}"))
}

/// The state the phone has now, fetched as a poll does; true once a fetch came and went.
fn polled(r: &Rig) {
    let control = r.link.control("pixel");
    let before = control.asked.load(Ordering::SeqCst);
    r.skip(POLL_MS);
    r.until("the fetch", |_| control.asked.load(Ordering::SeqCst) > before);
    r.pump();
}

#[test]
fn monitoring_fetch_emits_only_valid_battery_state() {
    let r = watched("monitoring-fetch");
    let control = r.link.control("pixel");
    *control.state.lock().unwrap() = Some(reads(80, false, 25.0));
    on_air(&r);
    polled(&r);
    assert_eq!(battery(&r), "80%");

    let mut no_battery = reads(80, false, 25.0);
    no_battery.as_object_mut().unwrap().remove("battery");
    *control.state.lock().unwrap() = Some(no_battery);
    polled(&r);
    assert_eq!(battery(&r), "80%");
    *control.state.lock().unwrap() = None;
    polled(&r);
    assert_eq!(battery(&r), "80%");
}

#[test]
fn monitoring_drops_a_reading_from_a_stream_that_ended() {
    let r = watched("monitoring-stale");
    on_air(&r);
    let old = r.session_id("pixel");
    r.stop(None);
    r.pump();
    r.start();
    r.pump();
    assert_ne!(r.session_id("pixel"), old);
    // Read before the restart, landed after
    r.w.phone_report(Report::State { source_id: "pixel".into(), session_id: old, state: reads(3, false, 25.0) });
    r.pump();
    assert_eq!(battery(&r), "—");
    assert_eq!(notes(&r), 0);
    r.phone_says("pixel", reads(60, false, 25.0));
    r.pump();
    assert_eq!(battery(&r), "60%");
}

#[test]
fn monitoring_poll_starts_daemon_fetch_thread() {
    let r = watched("monitoring-poll");
    let control = r.link.control("pixel");
    *control.state.lock().unwrap() = Some(reads(80, false, 25.0));
    on_air(&r);
    r.until("the stream's own first fetch", |r| battery(r) == "80%");
    let before = control.asked.load(Ordering::SeqCst);
    // A phone that doesn't answer: this thread carries on, the fetch waits on its own
    control.stalled.store(true, Ordering::SeqCst);
    *control.state.lock().unwrap() = Some(reads(55, false, 25.0));
    r.skip(POLL_MS);
    r.until("the fetch", |_| control.asked.load(Ordering::SeqCst) > before);
    assert_ne!(battery(&r), "55%");
    control.stalled.store(false, Ordering::SeqCst);
    r.until("the reading", |r| battery(r) == "55%"); // from that client's own fetch: the heat check that also asks shows nothing here

    // No client, no fetch
    r.stop(None);
    r.pump();
    let after = control.asked.load(Ordering::SeqCst);
    r.skip(POLL_MS);
    assert_eq!(control.asked.load(Ordering::SeqCst), after);
}

#[test]
fn monitoring_says_why_it_stopped_in_the_window_and_can_start_anyway() {
    let r = watched("monitoring-anyway");
    on_air(&r);
    r.configure("monitoring", json!({"battery_notify": false, "battery_stop": true})); // no notification: the banner still says it
    r.phone_says("pixel", reads(15, false, 30.0));
    r.until("the stop", |r| r.streaming().is_empty());
    let banner = r.banner("stopped:pixel").expect("a banner");
    assert_eq!(banner["title"], "The phone's battery is at 15%");
    assert!(banner["text"].contains("20%"), "{}", banner["text"]);
    assert_eq!(banner["actions"], "Start anyway");
    assert_eq!(notes(&r), 0);

    let (workers, before) = (r.workers_made(), starts(&r, "pixel"));
    press_first_button(&r, "stopped:pixel");
    r.until("this phone starts", |r| r.workers_made() > workers);
    assert_eq!(starts(&r, "pixel"), before + 1, "named, not whatever's picked");
    assert_eq!(r.streaming(), ["pixel"]);

    r.last_worker().says("ok", "Streaming"); // the start went through: still low, but it keeps going
    r.pump();
    r.phone_says("pixel", reads(14, false, 30.0));
    r.pump();
    assert_eq!(r.streaming(), ["pixel"]);
    r.phone_says("pixel", reads(30, true, 30.0)); // charged up past the limit: the next drop stops it again
    r.pump();
    r.phone_says("pixel", reads(15, false, 30.0));
    r.until("the stop", |r| r.streaming().is_empty());
}

#[test]
fn monitoring_start_anyway_for_heat_still_stops_for_battery() {
    let r = watched("monitoring-anyway-heat");
    on_air(&r);
    r.configure("monitoring", json!({"temp_stop": true, "battery_stop": true}));
    r.add("oneplus"); // pixel's settings stay with it, behind the panels
    r.last_worker().says("ok", "Streaming");
    r.pump();
    assert_eq!(r.shown().as_deref(), Some("oneplus"));

    r.phone_says("pixel", reads(80, false, 50.0));
    r.until("the stop", |r| r.streaming() == ["oneplus"]);
    let banner = r.banner("stopped:pixel").expect("a banner");
    assert_eq!(banner["title"], "Pixel is at 50.0 °C");
    assert!(banner["text"].contains("45 °C"), "{}", banner["text"]);

    let (workers, before) = (r.workers_made(), starts(&r, "pixel"));
    press_first_button(&r, "stopped:pixel");
    r.until("this phone starts", |r| r.workers_made() > workers);
    assert_eq!(starts(&r, "pixel"), before + 1);
    r.last_worker().says("ok", "Streaming"); // streaming again
    r.pump();
    assert!(r.streaming().contains(&"pixel".to_string()));

    r.phone_says("pixel", reads(80, false, 50.0));
    r.pump();
    assert!(r.streaming().contains(&"pixel".to_string()), "still hot, but started anyway");
    r.phone_says("pixel", reads(10, false, 50.0));
    r.until("the stop", |r| !r.streaming().contains(&"pixel".to_string()));
}

#[test]
fn h264_too_much_for_the_encoder_stops_and_offers_heavy() {
    let r = with("encoder-too-much", Box::new(StreamOutputPlugin::new(true)));
    on_air(&r);
    r.add("oneplus");
    r.last_worker().says("ok", "Streaming");
    r.pump();
    assert_eq!(r.shown().as_deref(), Some("oneplus"));
    let format = || r.w.state.borrow().plugin_config("stream_output").unwrap()["format"].clone();
    assert_eq!(format(), "h264");

    r.phone_says("oneplus", state_with(json!({"codecs": ["mjpeg", "h264"], "codec": "mjpeg",
        "codec_error": "H.264 isn't available at 4096x3072 on this phone", "codec_unsupported": true})));
    r.until("the stop", |r| r.streaming() == ["pixel"]);
    assert_eq!(r.link.logged("reconnect"), 0);
    let banner = r.banner("encoder").expect("a banner");
    assert_eq!(banner["title"], "Too much for the phone's H.264 encoder");
    assert!(banner["text"].starts_with("H.264 isn't available at 4096x3072") && banner["text"].contains("lower resolution or FPS"), "{}", banner["text"]);
    assert_eq!(banner["actions"], "Switch to Heavy");
    assert_eq!(r.shown().as_deref(), Some("pixel"));

    let (workers, before) = (r.workers_made(), starts(&r, "oneplus"));
    press_first_button(&r, "encoder");
    r.until("that phone starts", |r| r.workers_made() > workers);
    assert!(starts(&r, "oneplus") > before, "the phone it was about, with focus moved on");
    assert_eq!(starts(&r, "pixel"), 1);
    r.until("the switch", |_| format() == "mjpeg");
    // The switch reconnects it, as _set_format does, so it's on Heavy from here on
    r.until("it's back", |r| r.streaming().len() == 2 && r.streaming().contains(&"oneplus".to_string()));
    assert!(!r.link.formats.lock().unwrap().last().unwrap().h264);
}
