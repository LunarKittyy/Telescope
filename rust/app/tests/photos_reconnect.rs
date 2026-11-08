// A DSLR whose live view drops: the Photos card hides while the camera can't take photos and has to come back once the stream is
// back, whether the camera answers on the port it had or on a new one (a USB camera that re-enumerates gets a new device number).
// The real Webcam plugin, its source, its capture thread and the Photos plugin run against a fake camera; only the stream worker is
// the rig's, and the test does what the worker does: reads the feed, reopens it when it breaks and says so.
#![cfg(target_os = "linux")]
mod rig;

use rig::{Listener, Picker, Rig};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::event::Event;
use telescope_core::host::{Host, HostStills};
use telescope_core::plugin::{ConfigRejected, PluginInfo};
use telescope_core::ui::{CardSpec, UiEvent};
use telescope_core::media::{OpenStop, VideoFeed};
use telescope_core::plugin::Plugin;
use telescope_effects::photos::PhotosPlugin;
use telescope_outputs::webcam::{System, WebcamPlugin, SCAN_MS};
use telescope_outputs::webcam_capture::gphoto::Timing;
use telescope_outputs::webcam_capture::gphoto_lib::{open_model, Connect, Kind, Rig as CameraRig, Setting};
use telescope_outputs::webcams::{Choice, Device};
use telescope_shell::worker::WorkerEvent;

/// The Photos plugin as the window runs it: an optional plugin only starts when it's switched on in the settings, which this one is.
struct SwitchedOn(PhotosPlugin);

impl Plugin for SwitchedOn {
    fn info(&self) -> PluginInfo {
        PluginInfo { required: true, optional: false, ..self.0.info() }
    }
    fn setup(&mut self, host: &mut dyn Host) {
        self.0.setup(host)
    }
    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.0.on_plugins_ready(host)
    }
    fn shutdown(&mut self, host: &mut dyn Host) {
        self.0.shutdown(host)
    }
    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.0.on_event(host, event)
    }
    fn card(&mut self) -> Option<CardSpec> {
        self.0.card()
    }
    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        self.0.on_ui(host, event)
    }
    fn get_config(&self) -> Value {
        self.0.get_config()
    }
    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        self.0.set_config(cfg)
    }
}

const SOURCE: &str = "webcam:gphoto2-d5100";

/// What the fake camera is doing, and where it answers.
struct Camera {
    port: String,
    settings: HashMap<String, (bool, Vec<String>, String)>,
    /// Live view frames that fail from now on (u32::MAX: until told otherwise).
    fail_previews: u32,
    opens: u32,
    /// Nothing answers on any port (the camera is between unplugging and plugging).
    absent: bool,
}

struct Fake(Arc<Mutex<Camera>>);

impl CameraRig for Fake {
    fn preview(&mut self) -> Result<Vec<u8>, String> {
        let mut s = self.0.lock().unwrap();
        if s.fail_previews > 0 {
            if s.fail_previews != u32::MAX {
                s.fail_previews -= 1;
            }
            return Err("Device Busy".into());
        }
        drop(s);
        std::thread::sleep(Duration::from_millis(3));
        Ok(vec![0xFF, 0xD8, 0xFF, 0xD9])
    }
    fn read(&mut self, name: &str) -> Option<Setting> {
        let (ro, choices, value) = self.0.lock().unwrap().settings.get(name)?.clone();
        Some(Setting { kind: if choices.is_empty() { Kind::Toggle } else { Kind::Choice }, readonly: ro, value, choices })
    }
    fn write(&mut self, name: &str, value: &str) -> Result<(), String> {
        match self.0.lock().unwrap().settings.get_mut(name) {
            Some((false, _, v)) => {
                *v = value.into();
                Ok(())
            }
            _ => Err("no".into()),
        }
    }
    fn restart_live_view(&mut self) {}
    fn close(&mut self) {}
}

fn camera() -> Arc<Mutex<Camera>> {
    let choice = |l: &[&str], v: &str| (false, l.iter().map(|s| s.to_string()).collect::<Vec<_>>(), v.to_string());
    let mut settings = HashMap::new();
    settings.insert("imagequality".to_string(), choice(&["JPEG Fine", "NEF (Raw)"], "JPEG Fine"));
    settings.insert("iso".to_string(), choice(&["100", "200", "400", "800"], "400"));
    settings.insert("shutterspeed".to_string(), choice(&["0,0010s", "0,0250s", "1,0000s"], "0,0250s"));
    settings.insert("viewfinder".to_string(), (false, Vec::new(), "false".into()));
    Arc::new(Mutex::new(Camera { port: "usb:001,004".into(), settings, fail_previews: 0, opens: 0, absent: false }))
}

fn timing() -> Timing {
    Timing { restart: Duration::from_millis(5), restart_max: Duration::from_millis(20), give_up_runs: 40, silent: Duration::from_secs(5), grace: Duration::from_secs(1) }
}

/// The Webcam plugin with the D5100 plugged in at `location`, which opens the fake camera.
fn webcam(state: &Arc<Mutex<Camera>>, plugged: &Arc<Mutex<Vec<Device>>>) -> WebcamPlugin {
    let state = state.clone();
    let opener: telescope_outputs::webcam_capture::Opener = Arc::new(move |d: &Device, _: &Choice| {
        let state = state.clone();
        let connect: Connect = Box::new(move |port| {
            let mut s = state.lock().unwrap();
            if s.absent || port != s.port {
                return Err("Could not find the camera".into());
            }
            s.opens += 1;
            Ok(Box::new(Fake(state.clone())) as Box<dyn CameraRig>)
        });
        open_model(&d.location, &d.name, timing(), connect)
    });
    let system = System { opener, users: Arc::new(|_| String::new()), exists: Arc::new(|_| true) };
    let plugged = plugged.clone();
    WebcamPlugin::new(Box::new(move || Ok(plugged.lock().unwrap().clone())), Box::new(|_| None), system)
}

fn d5100(port: &str) -> Device {
    Device { key: "gphoto2-d5100".into(), name: "Nikon DSC D5100".into(), location: format!("gphoto2:{port}"), modes: Vec::new() }
}

struct Dslr {
    r: Rig,
    state: Arc<Mutex<Camera>>,
    plugged: Arc<Mutex<Vec<Device>>>,
    feed: Option<Box<dyn VideoFeed>>,
    photos: usize,
}

fn started(name: &str) -> Dslr {
    let state = camera();
    let plugged = Arc::new(Mutex::new(vec![d5100("usb:001,004")]));
    let (s, p) = (state.clone(), plugged.clone());
    let r = Rig::with(name, move |link, heard| {
        vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(Picker(link.clone())), Box::new(webcam(&s, &p)), Box::new(SwitchedOn(PhotosPlugin::new()))]
    });
    let photos = r.w.state.borrow().index("photos").unwrap();
    r.until("the webcam is offered", |r| r.w.streams.plugin_sources.borrow().contains_key(SOURCE));
    r.pick(SOURCE);
    r.emit(Event::SourceSelected { source_id: SOURCE.into() });
    r.start();
    let mut d = Dslr { r, state, plugged, feed: None, photos };
    d.open_feed();
    d.r.worker(0).says("ok", "Streaming");
    d.r.pump();
    d
}

impl Dslr {
    /// What the worker does: opens the source's feed (which waits for the first frame, as it does).
    fn open_feed(&mut self) {
        let opener = self.r.worker(0).opener.lock().unwrap().clone();
        let opened = opener(&OpenStop::default()).unwrap_or_else(|why| panic!("the feed didn't open: {why}"));
        self.feed = Some(opened.feed);
        self.r.pump();
    }

    /// What the worker does when the feed breaks: says it dropped, lets go of it, and opens another until one opens.
    fn reconnect(&mut self) {
        let w = self.r.worker(0);
        w.says("reconnecting", "Stream dropped - reconnecting");
        let old = self.feed.take();
        let opener = w.opener.lock().unwrap().clone();
        let until = Instant::now() + Duration::from_secs(10);
        let opened = loop {
            match opener(&OpenStop::default()) {
                Ok(opened) => break opened,
                Err(_) => {
                    assert!(Instant::now() < until, "never reconnected");
                    std::thread::sleep(Duration::from_millis(20));
                }
            }
        };
        drop(old);
        self.feed = Some(opened.feed);
        w.says("ok", "Stream reconnected");
        w.event(WorkerEvent::Reconnected);
        self.r.pump();
    }

    /// Reads until the feed breaks, as the worker's reader does.
    fn read_until_it_breaks(&mut self) {
        let until = Instant::now() + Duration::from_secs(10);
        while self.feed.as_mut().unwrap().read().is_ok() {
            assert!(Instant::now() < until, "the feed never broke");
        }
    }

    fn prop(&self, name: &str) -> slint_interpreter::Value {
        self.r.w.ui.get_property(&format!("p{}-{name}", self.photos)).unwrap_or_else(|e| panic!("{name}: {e:?}"))
    }

    /// Whether the Photos card is up and can take a photo.
    fn card_shown(&self) -> bool {
        self.r.pump();
        self.prop("shown") == slint_interpreter::Value::Bool(true) && self.prop("take-enabled") == slint_interpreter::Value::Bool(true)
    }

    fn caps(&self) -> Option<Value> {
        let at = self.r.w.state.borrow().index("panel").unwrap();
        let caps = self.r.w.state.borrow_mut().call(at, |_, host| host.still_caps(None)).unwrap();
        self.r.w.process();
        caps
    }

    /// Lets the plugin scan for webcams, which is when it learns a camera has moved to another port.
    fn rescan(&self) {
        self.r.skip(SCAN_MS + 100);
    }

    fn wait_for(&self, what: &str, cond: impl Fn(&Dslr) -> bool) {
        let until = Instant::now() + Duration::from_secs(10);
        while !cond(self) {
            assert!(Instant::now() < until, "never happened: {what}");
            self.r.pump();
        }
    }
}

#[test]
fn a_dslr_streaming_shows_the_photos_card() {
    let d = started("photos-reconnect-start");
    assert!(d.caps().is_some_and(|c| c["camera"] == json!(true)));
    assert!(d.card_shown());
}

#[test]
fn the_card_hides_while_live_view_is_gone_and_is_back_when_the_same_thread_gets_the_camera_again() {
    let d = started("photos-reconnect-thread");
    // Frames fail and the camera can't be opened for a moment: the capture keeps trying and gets it again before it gives up
    {
        let mut s = d.state.lock().unwrap();
        s.fail_previews = u32::MAX;
        s.absent = true;
    }
    d.wait_for("the card to hide", |d| !d.card_shown());
    {
        let mut s = d.state.lock().unwrap();
        s.fail_previews = 0;
        s.absent = false;
    }
    d.wait_for("the card to come back", |d| d.card_shown());
    assert!(d.caps().is_some());
}

#[test]
fn the_card_comes_back_when_the_stream_reconnects_on_the_same_port() {
    let mut d = started("photos-reconnect-same-port");
    d.state.lock().unwrap().fail_previews = u32::MAX;
    d.read_until_it_breaks();
    d.wait_for("the card to hide", |d| !d.card_shown());
    // The camera is back, on the port it had, and the worker opens the feed again
    d.state.lock().unwrap().fail_previews = 0;
    d.reconnect();
    assert!(d.caps().is_some(), "the camera takes photos again");
    d.wait_for("the card to come back", |d| d.card_shown());
}

#[test]
fn the_card_comes_back_when_the_camera_answers_on_a_new_port() {
    let mut d = started("photos-reconnect-new-port");
    d.state.lock().unwrap().fail_previews = u32::MAX;
    d.read_until_it_breaks();
    d.wait_for("the card to hide", |d| !d.card_shown());
    // Unplugged and replugged by the camera itself (its power saving): the same camera, another device number
    {
        let mut s = d.state.lock().unwrap();
        s.port = "usb:001,009".into();
        s.fail_previews = 0;
    }
    *d.plugged.lock().unwrap() = vec![d5100("usb:001,009")];
    d.rescan();
    d.reconnect();
    assert!(d.caps().is_some(), "the camera takes photos again on its new port");
    d.wait_for("the card to come back", |d| d.card_shown());
}
