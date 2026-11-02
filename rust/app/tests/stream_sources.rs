// Streaming from something that isn't a phone: a plugin's source, a browser in these tests.
mod rig;

use rig::{FakeSource, Listener, Picker, Rig};
use serde_json::json;
use slint_interpreter::Value as SValue;
use std::sync::atomic::Ordering;
use telescope_core::event::Event;
use telescope_core::output::Outputs;
use telescope_core::plugin::Plugin;
use telescope_shell::worker::WorkerEvent;

fn with_mic(name: &str) -> Rig {
    Rig::with(name, |link, heard| {
        let mut mic = Listener::new("microphone", heard);
        mic.config = json!({"enabled": true});
        vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(mic), Box::new(Picker(link.clone()))]
    })
}

/// The browser picked and started.
fn browser(name: &str) -> Rig {
    let r = with_mic(name);
    r.offer(FakeSource::new("browser"));
    r.pick("browser");
    r.start();
    r
}

fn link_did(r: &Rig, what: &str) -> bool {
    r.link.log.lock().unwrap().iter().any(|l| l.starts_with(what))
}

fn changes(r: &Rig) -> Vec<String> {
    r.heard().into_iter().filter_map(|l| l.strip_prefix("panel camera_on_changed ").map(String::from)).collect()
}

fn tiles(r: &Rig) -> Vec<String> {
    r.rows("tiles").iter().map(|t| t["id"].clone()).collect()
}

#[test]
fn a_browser_camera_streams_with_its_camera_on() {
    let r = with_mic("browser-camera-on");
    r.offer(FakeSource::new("browser"));
    r.pick("browser");
    r.pump();
    assert!(!r.w.state.borrow().streams.can_turn_off.0, "only a phone can keep its mic going alone");
    r.w.set_camera_on(false, false);
    r.pump();
    assert!(r.host_says_camera_on() && changes(&r).is_empty());
    r.start();
    r.pump();
    assert_eq!(r.workers_made(), 1);
    assert!((r.worker(0).opener.lock().unwrap())(&Default::default()).is_ok(), "it reads from the source itself");
    assert!(!r.w.state.borrow().streams.can_turn_off.0);
}

#[test]
fn a_browser_starts_by_itself_and_goes_when_it_leaves() {
    let r = with_mic("browser-leaves");
    r.offer(FakeSource { goes_to: Some("browser:pixel".into()), ..FakeSource::new("browser") });
    r.offer(FakeSource::new("browser:pixel"));
    r.pick("browser");
    r.start(); // Start with Browser camera picked streams the browser that's there
    assert_eq!(r.streaming(), ["browser:pixel"]);
    assert_eq!(r.link.picked_now().as_deref(), Some("browser:pixel"));
    r.w.remove_plugin_source("browser:pixel");
    r.pump();
    assert!(r.streaming().is_empty());
    assert_eq!(r.link.picked_now().as_deref(), Some("browser"));
    assert!(!r.w.streams.plugin_sources.borrow().contains_key("browser:pixel"));

    r.offer(FakeSource::new("browser:ipad"));
    r.w.stream_plugin_source("browser:ipad"); // nothing streams: it's picked and streamed
    r.until("it streams", |r| r.streaming() == ["browser:ipad"]);
    assert_eq!(r.link.picked_now().as_deref(), Some("browser:ipad"));
    r.pump();
    let camera = r.w.state.borrow_mut().call(0, |_, host| telescope_core::host::HostStreams::stream_output(host, "browser:ipad")).unwrap();
    assert_eq!(camera, r.cameras.slots()[0].name);
}

#[test]
fn each_source_keeps_its_own_camera_choice() {
    let r = with_mic("source-camera-choice");
    r.offer(FakeSource::new("browser"));
    r.w.set_camera_on(false, false);
    r.pump();
    r.pick("browser");
    r.emit(Event::SourceSelected { source_id: "browser".into() });
    r.pump();
    assert!(r.host_says_camera_on());
    assert_eq!(changes(&r), ["false", "true"]);
    assert_eq!(r.text("start-text"), "Start streaming");
    r.pick("pixel");
    r.emit(Event::SourceSelected { source_id: String::new() });
    r.pump();
    assert!(!r.host_says_camera_on());
    assert_eq!(changes(&r), ["false", "true", "false"]);
    assert_eq!(r.text("start-text"), "Start mic only");
}

#[test]
fn a_dropped_browser_stream_waits_for_it_without_looking_for_a_phone() {
    let r = browser("browser-dropped");
    r.worker(0).says("reconnecting", "Stream dropped - reconnecting");
    r.pump();
    assert_eq!(r.times_heard("panel lost browser"), 1);
    assert!(!link_did(&r, "recovery_check"), "no phone to look for");
    assert_eq!(r.streaming(), ["browser"]);
    r.stop(None);
    r.pump();
    assert!(!link_did(&r, "stop "), "nothing on a phone to stop");
}

#[test]
fn a_browser_gone_a_while_gets_the_wait_screen_and_a_banner() {
    let r = browser("browser-gone");
    r.worker(0).says("reconnecting", "Stream dropped - reconnecting");
    r.pump();
    r.skip(10_000);
    assert!(r.worker(0).is_parked());
    r.skip(20_000);
    let banner = r.banner("stopped:browser").expect("a banner");
    assert_eq!(banner["title"], "Can't reach Browser camera");
    assert_eq!(banner["text"], "Open the Telescope page in it again.");
    let starting = r.times_heard("microphone starting");
    r.worker(0).event(WorkerEvent::Reconnected);
    r.pump();
    assert_eq!(r.times_heard("microphone starting"), starting + 1, "the wait screen lets go before the camera is taken back");
    assert!(!r.worker(0).is_parked());
    assert!(r.banner("stopped:browser").is_none());
}

#[test]
fn a_browser_camera_on_auto_canvas_streams_at_its_own_default_size() {
    let r = with_mic("browser-canvas");
    r.offer(FakeSource::new("browser"));
    r.pick("browser");
    r.start();
    let expected = if cfg!(target_os = "linux") { (Some(1920), Some(1080)) } else { (None, None) };
    assert_eq!(r.worker(0).canvas, expected);
}

fn with_waiting_tile(name: &str) -> (Rig, std::sync::Arc<FakeSource>) {
    let r = with_mic(name);
    r.start();
    r.add("oneplus");
    r.pump();
    let waiting = r.offer(FakeSource { waits: true, ..FakeSource::new("waiting") });
    r.w.with_flow(|flow, host| flow.add_stream(host, "waiting"));
    r.pump();
    (r, waiting)
}

#[test]
fn a_source_that_waits_gets_a_tile_with_the_panels_on_it() {
    let (r, waiting) = with_waiting_tile("waiting-tile");
    assert_eq!(r.shown().as_deref(), Some("waiting"));
    assert_eq!(r.link.picked_now().as_deref(), Some("waiting"));
    assert_eq!(tiles(&r), ["pixel", "oneplus", "waiting"]);
    assert_eq!(r.tile("waiting").unwrap()["state"], "Waiting");
    assert!(!r.w.add_candidates().iter().any(|(id, _)| id == "waiting"));

    r.w.with_flow(|flow, host| flow.focus_stream(host, "pixel"));
    assert_eq!(r.shown().as_deref(), Some("pixel"));
    r.w.with_flow(|flow, host| flow.focus_stream(host, "waiting"));
    assert_eq!(r.shown().as_deref(), Some("waiting"));

    r.w.ui.invoke("tile-stop", &[SValue::String("waiting".into())]).unwrap(); // its tile's X
    r.pump();
    assert_eq!(waiting.cancels.load(Ordering::SeqCst), 1);
    assert_eq!(tiles(&r), ["pixel", "oneplus"]);
    assert_eq!(r.shown().as_deref(), Some("pixel"));
}

#[test]
fn stop_drops_a_waiting_tile_too() {
    let (r, waiting) = with_waiting_tile("waiting-stop");
    r.w.toggle_stream();
    r.pump();
    assert_eq!(waiting.cancels.load(Ordering::SeqCst), 1);
    assert!(!r.w.is_streaming());
    assert!(r.w.streams.flow.borrow().sessions.pending.is_none());
}
