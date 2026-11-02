// Browser camera in the real window: the real plugin and its sources, with a stand-in for its network server, as
// test_browser_camera.py's window tests.
mod rig;

use rig::{FakeLink, Listener, Picker, Rig};
use serde_json::{json, Value};
use slint_interpreter::Value as SValue;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use telescope_core::event::Event;
use telescope_core::host::Host;
use telescope_core::media::{Codec, FeedEnd, FeedStopper, Packet, VideoFeed};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::source::{ControlClient, StreamSource};
use telescope_core::web::ServerStats;
use telescope_link::browser::feed::BrowserHub;
use telescope_link::browser::plugin::{BrowserCameraPlugin, Serving, SOURCE_ID};

/// The real picker's part: the pick moves, and it tells the plugins.
struct Panels(Arc<FakeLink>);

impl Plugin for Panels {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "panels".into(), title: "panels".into(), ..Default::default() }
    }
    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        if let Event::SelectSource { source_id } | Event::SourceShown { source_id } = event {
            *self.0.picked.lock().unwrap() = Some(source_id.clone()).filter(|s| !s.is_empty());
            host.emit(Event::SourceSelected { source_id: source_id.clone() });
        }
    }
}

type OnChange = Arc<dyn Fn() + Send + Sync>;

/// The stand-in server: no network, and a browser arriving says so as the real one's threads do.
#[derive(Default)]
struct Server {
    stopped: AtomicBool,
    on_change: Mutex<Option<OnChange>>,
}

struct Running(Arc<Server>);

impl Serving for Running {
    fn start(&self) -> Result<(), String> {
        Ok(())
    }
    fn stop(&self) {
        self.0.stopped.store(true, Ordering::SeqCst);
    }
    fn url_for(&self, host: &str) -> String {
        format!("https://{host}:8767/#t1")
    }
    fn new_token(&self) {}
    fn stats(&self) -> Option<Arc<ServerStats>> {
        None
    }
}

struct Browsers {
    hub: Arc<BrowserHub>,
    launcher: Arc<dyn StreamSource>,
    server: Arc<Server>,
}

fn window(name: &str) -> (Rig, Browsers) {
    let parts: Arc<Mutex<Option<Browsers>>> = Arc::default();
    let filled = parts.clone();
    let rig = Rig::with(name, move |link, _| {
        let server = Arc::new(Server::default());
        let made = server.clone();
        let plugin = BrowserCameraPlugin::new(Box::new(move |_hub, on_change| {
            *made.on_change.lock().unwrap() = Some(on_change);
            Ok(Box::new(Running(made.clone())) as Box<dyn Serving>)
        }), || vec!["192.168.1.20".into()]);
        *filled.lock().unwrap() = Some(Browsers { hub: plugin.hub.clone(), launcher: plugin.launcher(), server });
        vec![Box::new(Panels(link.clone())) as Box<dyn Plugin>, Box::new(plugin)]
    });
    rig.pump();
    let parts = parts.lock().unwrap().take().unwrap();
    (rig, parts)
}

/// A browser connects and says what it runs on, as the server's thread would report it.
fn arrive(r: &Rig, b: &Browsers, bid: &str, device: &str) {
    let feed = b.hub.join(bid).expect("room for it");
    let gen = feed.connect();
    feed.note_hello(gen, device, "");
    let on_change = b.server.on_change.lock().unwrap().clone().expect("the server runs");
    on_change();
    r.pump();
}

fn pick(r: &Rig, id: &str) {
    r.pick(id);
    r.emit(Event::SourceSelected { source_id: id.into() });
    r.pump();
}

fn tiles(r: &Rig) -> Vec<String> {
    r.rows("tiles").iter().map(|t| t["id"].clone()).collect()
}

fn streams_from(r: &Rig, id: &str) -> bool {
    r.streaming().iter().any(|s| s == id)
}

#[test]
fn a_browser_takes_the_waiting_tile_over_next_to_a_phone() {
    let (r, b) = window("browser-takes-over");
    r.start();
    r.w.with_flow(|flow, host| flow.add_stream(host, SOURCE_ID));
    r.pump();
    assert_eq!(tiles(&r), ["pixel", SOURCE_ID]);

    arrive(&r, &b, "pixel-aaaaaaaa", "Chrome on Android");
    r.until("the browser streams", |r| streams_from(r, "browser:pixel-aaaaaaaa")); // it used to stop the server on the way, and itself
    r.pump();
    assert_eq!(tiles(&r), ["pixel", "browser:pixel-aaaaaaaa"]);
    assert!(r.w.streams.flow.borrow().sessions.pending.is_none());
    assert_eq!(r.shown().as_deref(), Some("browser:pixel-aaaaaaaa"));
    assert!(!b.server.stopped.load(Ordering::SeqCst));
    assert!(!b.launcher.waiting());
}

#[test]
fn every_browser_that_joined_during_a_phone_start_streams_after_it() {
    let (r, b) = window("browser-during-start");
    pick(&r, SOURCE_ID);
    arrive(&r, &b, "browser-initial1", "Chrome on Mac");
    r.until("the first browser streams", |r| r.streaming().len() == 1);

    r.link.hold_wakes(true);
    r.w.with_flow(|flow, host| flow.add_stream(host, "pixel"));
    r.until("the phone wakes", |r| r.w.streams.flow.borrow().sessions.waking);
    arrive(&r, &b, "browser-aaaaaaaa", "Chrome on Android");
    arrive(&r, &b, "browser-bbbbbbbb", "Safari on iOS");
    r.pump();
    assert_eq!(r.streaming().len(), 1, "they wait for the phone");

    r.link.hold_wakes(false);
    r.until("all four stream", |r| r.streaming().len() == 4);
    assert!(streams_from(&r, "browser:browser-aaaaaaaa") && streams_from(&r, "browser:browser-bbbbbbbb"));
}

#[test]
fn a_browser_that_joins_while_the_extra_cameras_are_set_up_streams_after() {
    let (r, b) = window("browser-during-slots");
    r.cameras.extras.store(0, Ordering::SeqCst);
    pick(&r, SOURCE_ID);
    arrive(&r, &b, "browser-aaaaaaaa", "Chrome");
    r.until("the first browser streams", |r| r.streaming().len() == 1);
    r.cameras.held.store(true, Ordering::SeqCst);
    arrive(&r, &b, "browser-bbbbbbbb", "Firefox"); // sets up the extra cameras
    r.w.slots_question().expect("the question").invoke("answered", &[SValue::Bool(true)]).unwrap();
    r.until("the cameras are being set up", |r| !r.cameras.asked.lock().unwrap().is_empty());
    arrive(&r, &b, "browser-cccccccc", "Safari"); // while that's under way
    r.cameras.held.store(false, Ordering::SeqCst);
    r.until("both stream", |r| streams_from(r, "browser:browser-bbbbbbbb") && streams_from(r, "browser:browser-cccccccc"));
}

/// _Source: a source that writes down what's asked of it.
#[derive(Default)]
struct Offered {
    opened: AtomicU32,
    state_asked: AtomicU32,
    sent: Mutex<Vec<Value>>,
}

struct Control(Arc<Offered>);

impl ControlClient for Control {
    fn send(&self, body: &Value) -> Result<(), String> {
        self.0.sent.lock().unwrap().push(body.clone());
        Ok(())
    }
    fn state(&self) -> Result<Value, String> {
        self.0.state_asked.fetch_add(1, Ordering::SeqCst);
        Err("no state".into())
    }
    fn has_audio(&self) -> bool {
        false
    }
}

struct NoPackets;

impl FeedStopper for NoPackets {
    fn stop(&self) {}
}

impl VideoFeed for NoPackets {
    fn read(&mut self) -> Result<Packet, FeedEnd> {
        Err(FeedEnd::Stopped)
    }
    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(NoPackets)
    }
}

struct Source(Arc<Offered>);

impl StreamSource for Source {
    fn id(&self) -> String {
        SOURCE_ID.into()
    }
    fn name(&self) -> String {
        "Browser camera".into()
    }
    fn url(&self) -> String {
        "browser:".into()
    }
    fn prepare(&self, _interactive: bool) -> bool {
        true
    }
    fn open_feed(&self) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        self.0.opened.fetch_add(1, Ordering::SeqCst);
        Ok((Codec::H264, Box::new(NoPackets)))
    }
    fn control_client(&self) -> Option<Box<dyn ControlClient>> {
        Some(Box::new(Control(self.0.clone())))
    }
    fn fps(&self) -> u32 {
        24
    }
}

#[test]
fn start_streams_from_the_source_without_waking_a_phone() {
    let r = Rig::with("source-start", |link, heard| {
        vec![Box::new(Listener::new("panel", heard)) as Box<dyn Plugin>, Box::new(Picker(link.clone()))]
    });
    let offered = Arc::new(Offered::default());
    r.w.add_plugin_source(Arc::new(Source(offered.clone())));
    let did = |r: &Rig, what: &str| r.link.log.lock().unwrap().iter().any(|l| l.starts_with(what));
    r.pick(SOURCE_ID);
    r.start();

    assert!(!did(&r, "wake"));
    let worker = r.worker(0);
    assert!((worker.opener.lock().unwrap())().is_ok());
    assert_eq!(offered.opened.load(Ordering::SeqCst), 1, "it reads from the source");
    assert_eq!((worker.fps, worker.width), (24, None));

    // Its control client is what the plugins' controls go to
    assert!(r.did_hear("panel start browser"));
    assert!(r.heard().iter().any(|l| l.starts_with("panel url browser: ")));
    let body = json!({"action": "torch", "value": 1});
    r.w.state.borrow_mut().call(0, |_, host| telescope_core::host::HostStreams::phone_control(host, None, &body));
    r.w.process();
    r.until("the control reaches the source", |_| !offered.sent.lock().unwrap().is_empty());
    assert_eq!(*offered.sent.lock().unwrap(), [body]);

    std::thread::sleep(Duration::from_millis(50));
    r.pump();
    assert_eq!(offered.state_asked.load(Ordering::SeqCst), 0, "no phone state to fetch");
    worker.says("reconnecting", "Stream dropped - reconnecting");
    r.pump();
    assert_eq!(offered.state_asked.load(Ordering::SeqCst), 0, "nowhere else to look for it");
    assert!(!did(&r, "recovery_check"));

    r.stop(None);
    r.pump();
    assert!(!did(&r, "stop "), "no phone to stop");
    assert!(r.streaming().is_empty());
}
