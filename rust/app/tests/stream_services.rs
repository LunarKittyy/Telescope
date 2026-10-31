// What a stream's phone offers plugins through the window: its sensors and its photos.
mod rig;

use rig::{state_with, Listener, Rig};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use telescope_core::host::Host;
use telescope_core::link::PhoneStills;
use telescope_core::media::PacketTap;
use telescope_core::plugin::Plugin;
use telescope_core::source::PairedPhone;
use telescope_link::sensors::SensorHub;
use telescope_shell::worker::VideoWorker;

fn asks<R>(r: &Rig, f: impl FnOnce(&mut dyn Host) -> R) -> R {
    let at = r.w.state.borrow().index("panel").unwrap();
    let answer = r.w.state.borrow_mut().call(at, |_, host| f(host)).unwrap();
    r.w.process();
    answer
}

fn status(r: &Rig, id: Option<&str>) -> Value {
    asks(r, |host| host.sensor_status(id))
}

#[test]
fn the_shown_streams_phone_state_tells_the_sensors_what_it_has() {
    let r = Rig::new("sensors-state");
    *r.link.sensors.lock().unwrap() = Some(Arc::new(SensorHub::new(|_| {})));
    asks(&r, |host| host.want_sensors("test", &["orientation".to_string()]));
    assert_eq!(status(&r, None)["state"], "off", "nothing streams");
    r.start();
    assert_eq!(status(&r, None)["state"], "starting");
    r.phone_says("pixel", state_with(json!({"sensors": ["orientation", "light"]})));
    r.pump();
    assert_eq!(status(&r, Some("pixel"))["kinds"], json!(["orientation", "light"]));
    r.phone_says("pixel", state_with(json!({}))); // an older phone
    r.pump();
    assert_eq!((status(&r, None)["state"].clone(), status(&r, None)["reason"].clone()), (json!("unavailable"), json!("old_phone")));
    assert!(r.did_hear("panel sensors pixel"), "{:?}", r.heard());
    r.stop(Some("pixel"));
    r.pump();
    assert_eq!(status(&r, None)["state"], "off");
    assert!(asks(&r, |host| host.sensors()).reading(None).is_none());
    asks(&r, |host| host.want_sensors("test", &[]));
}

type Taken = (Option<String>, Value, String);
type Later<T> = Mutex<Option<Box<T>>>;

/// The phone's photos, doing what a test says when it says.
#[derive(Default)]
struct Stills {
    caps: Mutex<Option<Value>>,
    busy: AtomicBool,
    taken: Mutex<Vec<Taken>>,
    progress: Later<dyn Fn(Value) + Send>,
    done: Later<dyn FnOnce(Value) + Send>,
    alive: Later<dyn Fn() -> bool + Send>,
}

impl PhoneStills for Stills {
    fn note_state(&self, _source_id: Option<&str>, state: &Value) {
        if let Some(stills) = state.get("stills_caps") {
            *self.caps.lock().unwrap() = Some(stills.clone());
        }
    }
    fn note_lens(&self, _source_id: Option<&str>, _camera: &Value) {}
    fn caps(&self, _source_id: Option<&str>) -> Option<Value> {
        self.caps.lock().unwrap().clone()
    }
    fn busy(&self, _source_id: Option<&str>) -> bool {
        self.busy.load(Ordering::SeqCst)
    }
    fn take(&self, source_id: Option<&str>, _stream_base: &str, _phone: &PairedPhone, options: &Value, folder: &str,
            on_progress: Box<dyn Fn(Value) + Send>, on_done: Box<dyn FnOnce(Value) + Send>, alive: Box<dyn Fn() -> bool + Send>) -> bool {
        self.taken.lock().unwrap().push((source_id.map(String::from), options.clone(), folder.into()));
        *self.progress.lock().unwrap() = Some(on_progress);
        *self.done.lock().unwrap() = Some(on_done);
        *self.alive.lock().unwrap() = Some(alive);
        true
    }
    fn shutdown(&self) {}
}

fn on_air_with_stills(name: &str) -> (Rig, Arc<Stills>) {
    let r = Rig::new(name);
    let stills = Arc::new(Stills::default());
    *r.link.stills.lock().unwrap() = Some(stills.clone());
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    (r, stills)
}

fn take(r: &Rig) -> bool {
    let asked = asks(r, |host| host.take_still(&json!({"format": "jpeg"}), "/tmp", None)).is_some();
    r.pump();
    asked
}

fn busy(r: &Rig) -> Vec<String> {
    r.heard().into_iter().filter(|l| l.starts_with("panel busy ")).collect()
}

#[test]
fn photos_follow_the_shown_streams_phone_and_tell_every_plugin() {
    let (r, stills) = on_air_with_stills("photos");
    assert!(asks(&r, |host| host.still_caps(None)).is_none());
    assert!(!take(&r), "nothing said yet");
    r.phone_says("pixel", state_with(json!({"stills_caps": {"formats": ["jpeg"]}})));
    r.pump();
    assert_eq!(asks(&r, |host| host.still_caps(None)).unwrap()["formats"], json!(["jpeg"]));

    r.forget_heard();
    assert!(take(&r));
    assert_eq!(*stills.taken.lock().unwrap(), [(Some("pixel".to_string()), json!({"format": "jpeg"}), "/tmp".to_string())]);
    assert_eq!(busy(&r), ["panel busy pixel true"]);
    (stills.progress.lock().unwrap().as_ref().unwrap())(json!({"state": "capturing"}));
    r.until("the stream is held", |r| r.worker(0).did("hold_for_photo 3"));
    stills.done.lock().unwrap().take().unwrap()(json!({"ok": true, "files": ["/tmp/IMG_1.jpg"]}));
    r.until("it reported back", |r| r.heard().iter().any(|l| l.starts_with("panel done true")));
    r.pump();
    assert_eq!(busy(&r), ["panel busy pixel true", "panel busy pixel false"]);
    assert!(r.did_hear("panel saved pixel [\"/tmp/IMG_1.jpg\"]"), "{:?}", r.heard());
}

#[test]
fn a_photo_ending_while_the_next_has_started_stays_busy() {
    let (r, stills) = on_air_with_stills("photos-next");
    r.phone_says("pixel", state_with(json!({"stills_caps": {"formats": ["jpeg"]}})));
    r.pump();
    take(&r);
    r.forget_heard();
    stills.busy.store(true, Ordering::SeqCst);
    stills.done.lock().unwrap().take().unwrap()(json!({"ok": true, "files": []}));
    r.until("it reported back", |r| r.heard().iter().any(|l| l.starts_with("panel done true")));
    r.pump();
    assert_eq!(busy(&r), ["panel busy pixel true"]);
    assert!(!take(&r), "one at a time");
}

#[test]
fn a_restart_ends_the_photo_on_the_old_session() {
    let (r, stills) = on_air_with_stills("photos-restart");
    r.phone_says("pixel", state_with(json!({"stills_caps": {"formats": ["jpeg"]}})));
    r.pump();
    take(&r);
    assert!((stills.alive.lock().unwrap().as_ref().unwrap())());
    r.w.toggle_stream();
    r.pump();
    assert!(!(stills.alive.lock().unwrap().as_ref().unwrap())());
}

#[test]
fn auto_framing_on_one_camera_doesnt_zoom_the_next_one_out() {
    use telescope_core::ui::UiEvent;
    use telescope_effects::framing::FramingPlugin;
    use telescope_effects::transforms::TransformsPlugin;
    let r = Rig::with("framing-two", |link, heard| {
        let list: Vec<Box<dyn Plugin>> = vec![Box::new(Listener::new("panel", heard)), Box::new(TransformsPlugin::new()), Box::new(rig::Picker(link.clone()))];
        list
    });
    r.w.state.borrow_mut().register(Box::new(FramingPlugin::with_clock(|| 10.0))).unwrap(); // optional, so it isn't loaded with the others
    let zoom = |r: &Rig| r.w.state.borrow().view.zoom;
    let focus = |r: &Rig, id: &str| {
        r.w.with_flow(|flow, host| flow.focus_stream(host, id));
        r.pump();
    };
    r.start();
    r.add("oneplus");
    r.until("the second stream", |r| r.workers_made() == 2);
    r.pump();
    assert_eq!(r.shown().as_deref(), Some("oneplus"));
    r.w.state.borrow_mut().ui_event("transforms", UiEvent { ui: "card".into(), callback: "zoom-moved".into(), args: vec![json!(2.0)] });
    r.pump();
    assert_eq!(zoom(&r), 2.0, "the desk camera, zoomed in by hand");
    focus(&r, "pixel");
    r.w.state.borrow_mut().ui_event("framing", UiEvent { ui: "card".into(), callback: "switch-toggled-by-hand".into(), args: vec![json!(true)] });
    r.pump();
    asks(&r, |host| host.steer_view("Auto framing", 1.5, 0.0, 0.0)); // framing someone on the face camera
    r.pump();
    assert_eq!(zoom(&r), 1.5, "{:?}", r.w.state.borrow().plugin_config("framing"));
    focus(&r, "oneplus");
    assert_eq!(r.w.state.borrow().plugin_config("framing").unwrap()["on"], false);
    assert_eq!(zoom(&r), 2.0);
    r.w.stop_all_streams();
}

#[test]
fn re_pairing_a_background_phone_restarts_it_with_the_new_token() {
    let r = Rig::new("repair-background");
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    r.add("oneplus");
    r.until("the second stream", |r| r.workers_made() == 2);
    r.worker(1).says("ok", "Streaming");
    r.pump();
    assert_eq!(r.shown().as_deref(), Some("oneplus"));
    r.link.set_token("pixel", "new-token");
    asks(&r, |host| host.reconnect_source("pixel"));
    r.until("it's back", |r| r.workers_made() == 3);
    r.pump();
    assert_eq!(r.shown().as_deref(), Some("pixel"));
    assert!(r.worker(0).is_stopped());
    assert!(!r.worker(1).is_stopped(), "the other stream stays");
    assert_eq!(r.link.logged("open pixel with new-token"), 1);
}

#[test]
fn packet_steps_coming_on_or_off_ask_the_phone_for_a_keyframe() {
    struct Nothing;
    impl telescope_core::host::PacketStepFactory for Nothing {
        fn make(&self) -> Box<dyn telescope_core::host::PacketStep> {
            unimplemented!("no packets in this test")
        }
    }
    let r = Rig::new("packets");
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    let control = r.link.control("pixel");
    let keyframes = || control.sent.lock().unwrap().iter().filter(|s| s["action"] == "keyframe").count();
    let on = r.worker(0).packets.clone();
    asks(&r, |host| host.change_packets("fun", Some(Arc::new(Nothing))));
    assert!(on.on());
    assert_eq!(keyframes(), 1);
    asks(&r, |host| host.change_packets("fun", None));
    assert!(!on.on());
    assert_eq!(keyframes(), 2);
    asks(&r, |host| host.change_packets("fun", None));
    assert_eq!(keyframes(), 2, "nothing came on or off");
}

#[test]
fn a_preview_frame_from_the_last_stream_isnt_shown_on_the_next() {
    let r = Rig::new("preview-last");
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    r.add("oneplus");
    r.until("the second stream", |r| r.workers_made() == 2);
    r.pump();
    r.w.with_flow(|flow, host| flow.focus_stream(host, "pixel"));
    r.pump();
    r.worker(0).slot().publish(rig::frame()); // came in before the switch
    r.w.with_flow(|flow, host| flow.focus_stream(host, "oneplus"));
    r.pump();
    assert!(r.w.preview_frame().is_none());
}

#[test]
fn stopping_a_stream_lets_go_of_its_colour_lut() {
    use telescope_core::colour::{lattice, ColourPipeline};
    use telescope_core::host::ColourStage;
    use std::time::Duration;
    let r = Rig::new("colour-let-go");
    r.w.state.borrow_mut().colour = ColourPipeline::inline();
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    let stage = ColourStage::new(2, Arc::new(lattice(2).iter().map(|v| 1.0 - v).collect()), None);
    r.w.state.borrow_mut().colour.set_stage("grade", Some(stage), 0, Some("pixel"));
    r.w.state.borrow_mut().pipeline_steps(Some("pixel"));
    assert!(r.w.state.borrow().colour.settle(Duration::from_secs(5)));
    assert!(r.w.state.borrow().colour.holds("pixel"));
    r.stop(Some("pixel"));
    r.pump();
    assert!(!r.w.state.borrow().colour.holds("pixel"));
}

#[test]
fn stopping_a_stream_forgets_who_it_was_tracking() {
    #[derive(Default)]
    struct Tracks(Mutex<Vec<String>>);
    impl telescope_core::vision::VisionService for Tracks {
        fn attach(&self, _bus: telescope_core::BusSender) {}
        fn state(&self) -> String {
            String::new()
        }
        fn load(&self) {}
        fn person_mask(&self, _source_id: &str, _frame: &telescope_core::Frame) -> Option<Arc<Vec<u8>>> {
            None
        }
        fn want_people(&self, _owner: &str, _on: bool) {}
        fn wants_people(&self) -> bool {
            false
        }
        fn feed_raw(&self, _source_id: &str, _frame: &telescope_core::Frame, _upright: telescope_core::vision::Upright) {}
        fn tracked_people(&self, _source_id: &str) -> (Vec<Value>, u32) {
            (Vec::new(), 0)
        }
        fn view_turned(&self, _old: telescope_core::vision::Turn, _new: telescope_core::vision::Turn) {}
        fn drop_stream(&self, source_id: &str) {
            self.0.lock().unwrap().push(source_id.to_string());
        }
        fn set_power_saving(&self, _on: bool) {}
        fn diagnostics(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn shutdown(&self) {}
        fn pack(&self) -> telescope_core::vision::VisionPack {
            Default::default()
        }
        fn download(&self, _switch_on: &str) {}
        fn cancel_download(&self) {}
        fn remove_pack(&self) {}
        fn download_failed(&self, _error: &str, _plugin: &str) {}
    }
    let r = Rig::new("tracking-forgot");
    let vision = Arc::new(Tracks::default());
    r.w.state.borrow_mut().vision = Some(vision.clone());
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    r.stop(Some("pixel"));
    r.pump();
    assert_eq!(*vision.0.lock().unwrap(), ["pixel"]);
}
