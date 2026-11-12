// Streaming the mic alone: the camera goes off and comes back while the stream carries on.
mod rig;

use rig::{state_with, Listener, Rig};
use serde_json::{json, Value};
use slint_interpreter::Value as SValue;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use telescope_core::event::Event;
use telescope_core::frame::PixelFormat;
use telescope_core::host::{Host, HostStreams};
use telescope_core::link::{PhoneStatus, Resolution};
use telescope_core::output::{Held, Outputs, Slot, VideoOutput, WaitScreen};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::source::RouteSpec;
use telescope_outputs::wait_screen_plugin::WaitScreenPlugin;
use telescope_shell::plugins::preview::Preview;
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

/// The real Preview next to a mic, as what's asked is what it shows.
fn previewing(name: &str, enabled: bool) -> Rig {
    Rig::with(name, move |_, heard| {
        let mut mic = Listener::new("microphone", heard);
        mic.config = json!({"enabled": enabled});
        vec![Box::new(Preview::default()) as Box<dyn Plugin>, Box::new(mic)]
    })
}

/// A plugin that turns the camera off from its tray action and says what it reads back.
struct Asks(Arc<Mutex<Vec<bool>>>);

impl Plugin for Asks {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "asks".into(), title: "Asks".into(), ..Default::default() }
    }
    fn on_tray_action(&mut self, host: &mut dyn Host, _action: &str) {
        host.set_camera_on(false, false);
        self.0.lock().unwrap().push(host.is_camera_on());
    }
}

fn sent(r: &Rig) -> Vec<Value> {
    r.link.control("pixel").actions().into_iter().filter(|(a, _)| a == "camera_on").map(|(_, v)| v).collect()
}

/// The mic's camera hooks, in order.
fn mic_calls(r: &Rig) -> Vec<String> {
    r.heard().into_iter().filter_map(|l| match l.as_str() {
        "microphone starting" => Some("starting".to_string()),
        "microphone camera off" => Some("off".to_string()),
        "microphone camera on" => Some("on".to_string()),
        _ => None,
    }).collect()
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
    assert_eq!(mic_calls(&r), ["starting"]);
    r.w.set_camera_on(false, false);
    r.pump();
    assert_eq!(sent(&r), [json!(0)]);
    assert!(r.worker(0).stops() && r.source("pixel").session().unwrap().worker.is_none());
    assert_eq!(r.streaming(), ["pixel"]);
    assert!(!r.host_says_camera_on());
    assert_eq!(mic_calls(&r), ["starting", "off"]);
    assert_eq!(changes(&r), ["false"]);
    assert_eq!(r.text("status"), "Streaming the mic, camera off");
    let control = r.link.control("pixel");
    let asked = control.asked.load(Ordering::SeqCst);
    r.skip(5_000);
    r.until("no video, so the phone is asked it's there", |_| control.asked.load(Ordering::SeqCst) == asked + 1);

    r.w.set_camera_on(true, false);
    r.pump();
    assert_eq!(sent(&r).last(), Some(&json!(1)));
    assert_eq!(r.workers_made(), 2);
    assert!(r.source("pixel").session().unwrap().worker.is_some());
    assert_eq!((r.w.workers_running(), r.worker(1).is_stopped()), (1, false), "the new worker is the stream's");
    assert_eq!(mic_calls(&r)[2..], ["starting", "on"], "the wait screen lets go before the new worker opens the camera");
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
    assert_eq!(mic_calls(&r), ["off"]);
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
    assert!(r.source("pixel").session().unwrap().worker.is_none() && r.worker(1).stops());
    assert_eq!(changes(&r), ["false", "true", "false"]);
    assert!(r.banner("camera").is_some());
}

#[test]
fn the_preview_follows_the_camera_off_and_back_on() {
    let r = previewing("camera-preview", true);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    assert!(r.flag("p0-camera-on") && !r.flag("p0-camera-off"));
    r.w.set_camera_on(false, false);
    r.pump();
    assert!(!r.flag("p0-camera-on") && r.flag("p0-camera-off"));
    r.w.set_camera_on(true, false);
    r.pump();
    assert!(r.flag("p0-camera-on") && !r.flag("p0-camera-off"), "paint_preview turns frames away otherwise");
}

#[test]
fn hooks_and_events_about_the_camera_read_it_as_it_is_now() {
    let r = on_air("camera-hooks-read");
    r.forget_heard();
    r.w.set_camera_on(false, false);
    r.pump();
    assert!(r.did_hear("panel sees streaming true camera false"), "{:?}", r.heard());
    assert!(r.did_hear("panel sees camera false can turn off true"), "{:?}", r.heard());
    r.forget_heard();
    r.w.set_camera_on(true, false);
    r.pump();
    assert!(r.did_hear("panel sees camera true can turn off true"), "{:?}", r.heard());
}

#[test]
fn the_mic_coming_on_lets_the_preview_turn_the_camera_off() {
    let r = previewing("camera-mic-enables", false);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    assert!(!r.flag("p0-camera-enabled"), "nothing would stream with the camera off");
    {
        let mut state = r.w.state.borrow_mut();
        let mic = state.index("microphone").unwrap();
        state.slots[mic].plugin.as_mut().unwrap().set_config(&json!({"enabled": true})).unwrap();
    }
    r.emit(Event::MicChanged { enabled: true, muted: false });
    r.pump();
    assert!(r.flag("p0-camera-enabled"));
}

#[test]
fn a_plugin_reads_back_the_camera_it_just_turned_off() {
    let said = Arc::<Mutex<Vec<bool>>>::default();
    let asks = Asks(said.clone());
    let r = Rig::built_tray("camera-reads-back", move |_, heard| {
        let mut mic = Listener::new("microphone", heard);
        mic.config = json!({"enabled": true});
        vec![Box::new(mic) as Box<dyn Plugin>, Box::new(asks)]
    });
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    r.tray.lock().unwrap().clicks.push("asks/off".into());
    r.w.poll_tray();
    r.pump();
    assert_eq!(*said.lock().unwrap(), [false]);
    assert!(!r.host_says_camera_on());
    assert_eq!(sent(&r), [json!(0)]);
}

#[test]
fn the_banners_try_again_turns_the_camera_back_on() {
    let r = on_air("camera-try-again");
    r.w.set_camera_on(false, false);
    r.pump();
    r.w.set_camera_on(true, false);
    r.pump();
    let session_id = r.session_id("pixel");
    r.w.phone_report(Report::CameraCheck { source_id: "pixel".into(), session_id,
                                           state: Some(json!({"camera_toggle": true, "camera_off": true, "camera_error": "Another app may be using it."})) });
    r.pump();
    assert!(!r.host_says_camera_on());
    r.w.ui.invoke("banner-action", &[SValue::String("camera".into()), SValue::Number(0.0)]).unwrap();
    r.pump();
    assert!(r.host_says_camera_on());
    assert_eq!(sent(&r).last(), Some(&json!(1)));
    assert_eq!(r.workers_made(), 3);
    assert!(r.banner("camera").is_none());
}

#[test]
fn a_mic_stream_that_moves_route_says_so_and_the_mic_goes_with_it() {
    let r = on_air("camera-off-moves");
    r.w.set_camera_on(false, false);
    r.pump();
    let wifi = RouteSpec::Network { address: "192.168.1.20".into() };
    r.link.answers_recovery("pixel", Resolution { route: Some(wifi.clone()), streaming: true, ..Resolution::new(PhoneStatus::Ready) });
    let session_id = r.session_id("pixel");
    r.forget_heard();
    r.w.phone_report(Report::Alive { source_id: "pixel".into(), session_id, ok: false });
    r.w.phone_report(Report::Alive { source_id: "pixel".into(), session_id, ok: false });
    r.until("the mic moved", |r| r.did_hear("microphone start pixel"));
    assert_eq!(r.link.logged(&format!("adopt pixel {wifi:?}")), 1);
    assert_eq!(r.source("pixel").session().unwrap().url, "https://192.168.1.20:8080/v1/video.h264");
    assert_eq!(r.text("status"), "Streaming the mic, camera off");
}

#[test]
fn mic_only_session_can_hand_over_to_usb() {
    const USB: &str = "https://usb-SER:8080/v1/video";
    let r = with_mic("camera-off-hand-over", true);
    r.pump();
    r.w.set_camera_on(false, false);
    r.pump();
    r.w.toggle_stream();
    r.until("the stream", |r| r.streaming() == ["pixel"]);
    *r.link.prepared.lock().unwrap() = Some(USB.into());
    r.forget_heard();
    let at = r.w.state.borrow().index("panel").unwrap();
    let usb = RouteSpec::Usb { serial: "SER".into() };
    assert!(r.w.state.borrow_mut().call(at, |_, host| host.hand_over_stream(&usb, Some("pixel"))).unwrap());
    r.w.process();
    r.until("the mic moved", |r| r.did_hear("microphone start pixel"));
    let session = r.source("pixel").session().unwrap();
    assert_eq!(session.url, USB);
    assert!(session.worker.is_none());
    assert_eq!(r.workers_made(), 0);
    assert_eq!(r.link.logged("commit pixel"), 1);
    assert_eq!(*r.link.control("pixel").retargeted.lock().unwrap(), [USB]);
    assert_eq!(r.text("status"), "Streaming the mic, camera off");
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
    alive(false); // the count started over
    r.pump();
    assert!(!r.source("pixel").recovering());
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
    r.w.with_flow(|flow, host| flow.reconnect_stream(host, None));
    r.until("both stream again", |r| r.streaming().len() == 2);
    r.pump();
    assert_eq!(r.w.streams.flow.borrow().sessions.main.as_ref().map(|m| m.id_str().to_string()).as_deref(), Some("pixel"));
    assert_eq!(r.times_heard("microphone start pixel"), 1, "{:?}", r.heard());
    assert!(r.heard().iter().any(|l| l.starts_with("microphone url https://pixel:8080/v1/video.h264 ")), "{:?}", r.heard());
}

/// The extra cameras' wait screens: which slots were shown and stopped.
#[derive(Default)]
struct Screens {
    shown: Arc<Mutex<Vec<u32>>>,
    stopped: Arc<Mutex<Vec<u32>>>,
}

struct Screen {
    slot: u32,
    shown: Arc<Mutex<Vec<u32>>>,
    stopped: Arc<Mutex<Vec<u32>>>,
}

impl Held for Screen {
    fn start(&self) {}
    fn stop(&self) {
        self.stopped.lock().unwrap().push(self.slot);
    }
    fn running(&self) -> bool {
        true
    }
}

impl WaitScreen for Screen {
    fn show(&self, _width: u32, _height: u32, _path: Option<&str>, _mirror: bool) {
        self.shown.lock().unwrap().push(self.slot);
    }
    fn set_watched(&self, _watched: bool) {}
}

impl Outputs for Screens {
    fn slots(&self) -> Vec<Slot> {
        Vec::new()
    }
    fn formats(&self) -> Vec<PixelFormat> {
        vec![PixelFormat::I420]
    }
    fn open(&self, _slot: u32, _width: u32, _height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        Err("no camera".into())
    }
    fn wait_screen(&self, slot: u32) -> Option<Box<dyn WaitScreen>> {
        Some(Box::new(Screen { slot, shown: self.shown.clone(), stopped: self.stopped.clone() }))
    }
}

// test_wait_screen.py::test_a_camera_off_stream_on_an_extra_camera_gets_its_wait_screen
#[test]
fn a_camera_off_stream_on_an_extra_camera_gets_its_wait_screen() {
    let screens = Arc::new(Screens::default());
    let outputs: Arc<dyn Outputs> = screens.clone();
    let r = Rig::with("camera-off-extra", move |_, heard| {
        let mut mic = Listener::new("microphone", heard);
        mic.config = json!({"enabled": true});
        vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(mic), Box::new(WaitScreenPlugin::new(outputs))]
    });
    r.start();
    r.add("oneplus");
    r.pump();
    r.stop(Some("pixel"));
    r.pump();
    assert_eq!(r.streaming(), ["oneplus"]);
    assert_eq!(r.source("oneplus").slot(), 1);
    let idle = |r: &Rig| serde_json::from_str::<Vec<u32>>(&r.idle_outputs()).unwrap();
    let count = |list: &Mutex<Vec<u32>>| list.lock().unwrap().iter().filter(|s| **s == 1).count();
    let shown = count(&screens.shown);
    r.w.set_camera_on(false, false);
    r.pump();
    assert!(idle(&r).contains(&1), "{}", r.idle_outputs());
    assert!(count(&screens.shown) > shown);
    let stops = count(&screens.stopped);
    r.w.set_camera_on(true, false);
    r.pump();
    assert!(!idle(&r).contains(&1), "the camera has it back: {}", r.idle_outputs());
    assert!(count(&screens.stopped) > stops);
}

#[test]
fn the_mic_switched_on_from_its_card_enables_the_camera_button_at_once() {
    let r = previewing("camera-mic-card", false);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    assert!(!r.flag("p0-camera-enabled"));
    {
        let mut state = r.w.state.borrow_mut();
        let mic = state.index("microphone").unwrap();
        state.slots[mic].plugin.as_mut().unwrap().set_config(&json!({"enabled": true})).unwrap();
    }
    // A card's callback delivers its events straight away, before the window reads the camera again
    r.w.state.borrow_mut().bus.emit(Event::MicChanged { enabled: true, muted: false });
    r.w.state.borrow_mut().deliver_events();
    r.w.process();
    r.pump();
    assert!(r.flag("p0-camera-enabled"), "the button stayed disabled with the mic on");
}

#[test]
fn the_preview_button_follows_the_camera_off_and_on_with_the_mic_running() {
    let r = previewing("camera-button-cycle", true);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    assert!(r.flag("p0-camera-enabled") && r.flag("p0-camera-on"));
    r.w.set_camera_on(false, false);
    r.pump();
    assert!(r.flag("p0-camera-enabled") && !r.flag("p0-camera-on"), "off stays clickable so the camera can come back");
    r.w.set_camera_on(true, false);
    r.pump();
    assert!(r.flag("p0-camera-enabled") && r.flag("p0-camera-on"));
    assert_eq!(r.streaming(), ["pixel"]);
    assert!(r.source("pixel").session().unwrap().worker.is_some());
}

#[test]
fn the_preview_button_follows_a_second_camera_starting() {
    let r = previewing("camera-button-two", true);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    assert!(r.flag("p0-camera-enabled"));
    r.add("oneplus");
    r.pump();
    assert!(!r.flag("p0-camera-enabled"), "only while one camera streams");
}
