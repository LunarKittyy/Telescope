// Holding a stream's exposure and white balance (host.lock_exposure): a phone confirms it in its state, a webcam at once.
mod rig;

use rig::{state_with, FakeSource, Listener, Rig};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use telescope_core::host::HostCamera;
use telescope_core::media::{Codec, VideoFeed};
use telescope_core::plugin::Plugin;
use telescope_core::source::{ControlClient, StreamSource};

fn on_air(name: &str) -> Rig {
    let r = Rig::with(name, |_, heard| vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>]);
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    r
}

/// What the phone says: the current lens can (or can't) lock, and its lock is `lock`.
fn phone(lock: &str, lockable: bool) -> Value {
    let mut state = state_with(json!({"cameras": [{"id": "0", "label": "Back", "current": true, "hasOis": false, "isoMin": 50, "isoMax": 3200,
        "shutterMinNs": 1, "shutterMaxNs": 2, "supportsManualSensor": false, "supportsManualWB": false, "supportsManualFocus": false,
        "minFocusDistance": 0.0, "aeCompMin": -8, "aeCompMax": 8, "aeCompStep": 0.167, "supportsFlash": false, "hwLevel": "FULL",
        "supportsExposureLock": lockable}]}));
    if lock != "off" {
        state["exposure_lock"] = json!(lock);
    }
    state
}

fn locks(r: &Rig) -> Vec<String> {
    r.heard().into_iter().filter_map(|l| l.strip_prefix("panel lock ").map(String::from)).collect()
}

fn sent_locks(r: &Rig, id: &str) -> Vec<Value> {
    r.link.control(id).actions().into_iter().filter(|(a, _)| a == "exposure_lock").map(|(_, v)| v).collect()
}

/// A plugin asks the host, as the Colour grading card's Auto button will.
fn ask(r: &Rig, source_id: Option<&str>, on: bool) {
    r.w.state.borrow_mut().call(0, |_, host| if on { host.lock_exposure(source_id) } else { host.unlock_exposure(source_id) });
    r.w.process();
    r.pump();
}

fn reads(r: &Rig, source_id: Option<&str>) -> String {
    r.w.state.borrow_mut().call(0, |_, host| host.exposure_lock(source_id)).unwrap()
}

/// Lets the clock run so the host asks the phone, which answers with what the control holds.
fn let_time_pass(r: &Rig, ms: u64) {
    for _ in 0..(ms / 150) {
        r.skip(150);
    }
}

#[test]
fn a_lens_that_cannot_lock_is_unavailable_and_nothing_is_sent() {
    let r = on_air("lock-cannot");
    r.phone_says("pixel", phone("off", false));
    ask(&r, Some("pixel"), true);
    assert_eq!(locks(&r), ["pixel unavailable"]);
    assert!(sent_locks(&r, "pixel").is_empty());
    assert_eq!(reads(&r, Some("pixel")), "off");
}

#[test]
fn a_phone_that_has_not_said_what_it_can_do_is_unavailable() {
    let r = on_air("lock-unknown");
    ask(&r, Some("pixel"), true);
    assert_eq!(locks(&r), ["pixel unavailable"]);
    assert!(sent_locks(&r, "pixel").is_empty());
}

#[test]
fn the_lock_is_asked_of_the_phone_and_only_confirmed_when_it_says_so() {
    let r = on_air("lock-confirms");
    r.phone_says("pixel", phone("off", true));
    ask(&r, None, true);
    assert_eq!(sent_locks(&r, "pixel"), [json!("1")]);
    assert_eq!(locks(&r), ["pixel locking"]);
    assert_eq!(reads(&r, None), "locking");
    // The phone is still settling: asked again a few times, and nothing is claimed
    *r.link.control("pixel").state.lock().unwrap() = Some(phone("locking", true));
    let_time_pass(&r, 900);
    assert_eq!(locks(&r), ["pixel locking"]);
    // Now the camera confirms
    *r.link.control("pixel").state.lock().unwrap() = Some(phone("locked", true));
    r.until("the lock is confirmed", |r| locks(r).last().is_some_and(|l| l == "pixel locked"));
    assert_eq!(reads(&r, None), "locked");
    assert_eq!(sent_locks(&r, "pixel"), [json!("1")], "asked once");
}

#[test]
fn asking_again_while_held_changes_nothing() {
    let r = on_air("lock-twice");
    r.phone_says("pixel", phone("off", true));
    ask(&r, None, true);
    r.phone_says("pixel", phone("locked", true));
    r.pump();
    assert_eq!(locks(&r), ["pixel locking", "pixel locked"]);
    ask(&r, None, true);
    assert_eq!(sent_locks(&r, "pixel"), [json!("1")]);
    assert_eq!(locks(&r), ["pixel locking", "pixel locked", "pixel locked"], "it says where it stands");
}

#[test]
fn unlocking_tells_the_phone_and_the_plugins() {
    let r = on_air("lock-unlock");
    r.phone_says("pixel", phone("off", true));
    ask(&r, None, true);
    r.phone_says("pixel", phone("locked", true));
    r.pump();
    ask(&r, None, false);
    assert_eq!(sent_locks(&r, "pixel"), [json!("1"), json!("0")]);
    assert_eq!(locks(&r), ["pixel locking", "pixel locked", "pixel unlocked"]);
    assert_eq!(reads(&r, None), "off");
    ask(&r, None, false);
    assert_eq!(sent_locks(&r, "pixel").len(), 2, "nothing was held, so nothing is sent");
}

#[test]
fn a_lock_the_phone_never_confirms_is_let_go() {
    let r = on_air("lock-fails");
    r.phone_says("pixel", phone("off", true));
    *r.link.control("pixel").state.lock().unwrap() = Some(phone("off", true));
    ask(&r, None, true);
    let_time_pass(&r, 9_000);
    assert_eq!(locks(&r), ["pixel locking", "pixel failed"]);
    assert_eq!(sent_locks(&r, "pixel"), [json!("1"), json!("0")], "and the phone is told to let go");
    assert_eq!(reads(&r, None), "off");
}

#[test]
fn a_phone_that_lets_go_by_itself_is_noticed() {
    let r = on_air("lock-lets-go");
    r.phone_says("pixel", phone("off", true));
    ask(&r, None, true);
    *r.link.control("pixel").state.lock().unwrap() = Some(phone("locked", true));
    r.until("it locks", |r| reads(r, None) == "locked");
    *r.link.control("pixel").state.lock().unwrap() = Some(phone("off", true));
    r.until("it is noticed", |r| locks(r).last().is_some_and(|l| l == "pixel unlocked"));
    assert_eq!(reads(&r, None), "off");
}

#[test]
fn switching_lens_lets_go_at_once_on_both_sides() {
    let r = on_air("lock-lens");
    r.phone_says("pixel", phone("off", true));
    ask(&r, None, true);
    r.phone_says("pixel", phone("locked", true));
    r.pump();
    r.w.state.borrow_mut().call(0, |_, host| telescope_core::host::HostStreams::phone_control(host, None, &json!({"action": "camera", "id": "2"})));
    r.w.process();
    r.pump();
    assert_eq!(locks(&r), ["pixel locking", "pixel locked", "pixel unlocked"]);
    assert_eq!(sent_locks(&r, "pixel"), [json!("1"), json!("0")], "the phone is told too, in case it kept the lock");
}

#[test]
fn a_stream_that_stops_takes_its_lock_with_it() {
    let r = on_air("lock-stops");
    r.phone_says("pixel", phone("off", true));
    ask(&r, None, true);
    r.phone_says("pixel", phone("locked", true));
    r.pump();
    r.stop(None);
    r.pump();
    let_time_pass(&r, 600);
    assert_eq!(locks(&r).last().map(String::as_str), Some("pixel unlocked"));
    assert_eq!(reads(&r, Some("pixel")), "off");
}

// A stream source that isn't a phone, with controls of its own to lock, like a webcam.
struct Lockable {
    inner: FakeSource,
    can: bool,
    locked: Arc<AtomicBool>,
    releases: Arc<AtomicU32>,
}

struct Locker {
    can: bool,
    locked: Arc<AtomicBool>,
    releases: Arc<AtomicU32>,
}

impl ControlClient for Locker {
    fn send(&self, _body: &Value) -> Result<(), String> {
        Ok(())
    }
    fn state(&self) -> Result<Value, String> {
        Err("no phone".into())
    }
    fn has_audio(&self) -> bool {
        false
    }
    fn lock_exposure(&self, on: bool) -> bool {
        if !on {
            self.releases.fetch_add(1, Ordering::SeqCst);
            self.locked.store(false, Ordering::SeqCst);
            return true;
        }
        self.locked.store(self.can, Ordering::SeqCst);
        self.can
    }
}

impl StreamSource for Lockable {
    fn id(&self) -> String {
        self.inner.id()
    }
    fn name(&self) -> String {
        self.inner.name()
    }
    fn url(&self) -> String {
        self.inner.url()
    }
    fn prepare(&self, interactive: bool) -> bool {
        self.inner.prepare(interactive)
    }
    fn open_feed(&self) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        self.inner.open_feed()
    }
    fn control_client(&self) -> Option<Box<dyn ControlClient>> {
        Some(Box::new(Locker { can: self.can, locked: self.locked.clone(), releases: self.releases.clone() }))
    }
    fn fps(&self) -> u32 {
        30
    }
}

fn webcam(name: &str, can: bool) -> (Rig, Arc<AtomicBool>, Arc<AtomicU32>) {
    let r = Rig::with(name, |_, heard| vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>]);
    let (locked, releases) = (Arc::new(AtomicBool::new(false)), Arc::new(AtomicU32::new(0)));
    r.w.add_plugin_source(Arc::new(Lockable { inner: FakeSource::new("webcam:cam"), can, locked: locked.clone(), releases: releases.clone() }));
    r.pick("webcam:cam");
    r.start();
    r.pump();
    (r, locked, releases)
}

#[test]
fn a_webcam_that_can_lock_is_locked_at_once() {
    let (r, locked, _) = webcam("lock-webcam", true);
    ask(&r, Some("webcam:cam"), true);
    assert!(locked.load(Ordering::SeqCst));
    assert_eq!(locks(&r), ["webcam:cam locked"], "no waiting for a confirmation");
    assert_eq!(reads(&r, Some("webcam:cam")), "locked");
    assert_eq!(reads(&r, None), "locked", "it is the stream the panels show");
}

#[test]
fn a_webcam_that_cannot_lock_is_unavailable() {
    let (r, locked, _) = webcam("lock-webcam-cannot", false);
    ask(&r, None, true);
    assert!(!locked.load(Ordering::SeqCst));
    assert_eq!(locks(&r), ["webcam:cam unavailable"]);
    assert_eq!(reads(&r, None), "off");
}

#[test]
fn a_webcam_is_unlocked_on_request_and_when_its_stream_stops() {
    let (r, locked, releases) = webcam("lock-webcam-release", true);
    ask(&r, None, true);
    ask(&r, None, false);
    assert!(!locked.load(Ordering::SeqCst));
    assert_eq!(locks(&r), ["webcam:cam locked", "webcam:cam unlocked"]);

    ask(&r, None, true);
    let before = releases.load(Ordering::SeqCst);
    r.stop(None);
    r.pump();
    let_time_pass(&r, 600);
    assert!(releases.load(Ordering::SeqCst) > before, "the camera is given its auto controls back");
    assert!(!locked.load(Ordering::SeqCst));
    assert_eq!(locks(&r).last().map(String::as_str), Some("webcam:cam unlocked"));
}

#[test]
fn a_source_with_no_controls_at_all_is_unavailable() {
    let r = Rig::with("lock-no-controls", |_, heard| vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>]);
    r.offer(FakeSource::new("browser"));
    r.pick("browser");
    r.start();
    r.pump();
    ask(&r, None, true);
    assert_eq!(locks(&r), ["browser unavailable"]);
}

#[test]
fn with_no_stream_there_is_nothing_to_lock() {
    let r = Rig::with("lock-no-stream", |_, heard| vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>]);
    ask(&r, None, true);
    assert!(locks(&r).is_empty());
    assert_eq!(reads(&r, None), "off");
    assert!(r.w.state.borrow_mut().call(0, |_, host| host.is_camera_on()).unwrap());
}
