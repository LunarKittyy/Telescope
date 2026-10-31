// test_frames.py's window half: real streams through the real window, on a platform of our own that a test can pump.
use serde_json::{json, Value};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::config::{ConfigFile, CONFIG_FILENAME};
use telescope_core::event::Event;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::host::Host;
use telescope_core::link::{Link, PairingOffer, PhoneSession, RoutePrefs, Unreachable};
use telescope_core::media::{Codec, DecodeOptions, Decoded, Decoder, FeedEnd, FeedStopper, FrameReader, FramePipeline, Media, Packet, ReadFrame, Scaler, StreamFrames, VideoFeed};
use telescope_core::output::{Outputs, SetupAction, SetupResult, Slot, VideoOutput};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::services::Services;
use telescope_core::source::{ControlClient, PairedPhone, StreamSource};
use telescope_shell::{offscreen, window};

const SIZE: (u32, u32) = (64, 32);

struct Source(&'static str);

impl StreamSource for Source {
    fn id(&self) -> String {
        self.0.into()
    }
    fn name(&self) -> String {
        format!("Camera {}", self.0)
    }
    fn url(&self) -> String {
        "test:".into()
    }
    fn prepare(&self, _interactive: bool) -> bool {
        true
    }
    fn open_feed(&self) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        Ok((Codec::H264, Box::new(NoFeed)))
    }
    fn control_client(&self) -> Option<Box<dyn ControlClient>> {
        None
    }
    fn fps(&self) -> u32 {
        30
    }
}

struct Never;

impl FeedStopper for Never {
    fn stop(&self) {}
}

struct NoFeed;

impl VideoFeed for NoFeed {
    fn read(&mut self) -> Result<Packet, FeedEnd> {
        Err(FeedEnd::Stopped)
    }
    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(Never)
    }
}

struct Stopper(Arc<AtomicBool>);

impl FeedStopper for Stopper {
    fn stop(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

/// A grey picture until it's stopped.
#[derive(Default)]
struct Pictures(Arc<AtomicBool>);

impl FrameReader for Pictures {
    fn read(&mut self) -> Result<ReadFrame, FeedEnd> {
        std::thread::sleep(Duration::from_millis(5));
        if self.0.load(Ordering::SeqCst) {
            return Err(FeedEnd::Stopped);
        }
        let (w, h) = SIZE;
        let mut data = vec![9; (w * h) as usize];
        data.extend(vec![128; (w * h / 2) as usize]);
        Ok(ReadFrame { frame: Frame::new(w, h, PixelFormat::I420, data), bytes: 1, count: 1 })
    }
    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(Stopper(self.0.clone()))
    }
    fn hardware(&self) -> bool {
        false
    }
}

struct NoDecoder;

impl Decoder for NoDecoder {
    fn decode(&mut self, _packet: &Packet) -> Decoded {
        Decoded { newest: None, count: 0 }
    }
    fn hardware(&self) -> bool {
        false
    }
}

struct Fed {
    real: telescope_media::FfmpegMedia,
    store: Arc<telescope_media::frames::Frames>,
}

impl Media for Fed {
    fn decoder(&self, _codec: Codec, _options: &DecodeOptions) -> Result<Box<dyn Decoder>, String> {
        Ok(Box::new(NoDecoder))
    }
    fn scaler(&self) -> Box<dyn Scaler> {
        self.real.scaler()
    }
    fn frame_reader(&self, _feed: Box<dyn VideoFeed>, _decoder: Box<dyn Decoder>) -> Result<Box<dyn FrameReader>, String> {
        Ok(Box::new(Pictures::default()))
    }
    fn pipeline(&self) -> Result<Box<dyn FramePipeline>, String> {
        self.real.pipeline()
    }
    fn stream_frames(&self) -> Option<Arc<dyn StreamFrames>> {
        Some(self.store.clone())
    }
}

struct Cam;

impl VideoOutput for Cam {
    fn send(&mut self, _frame: &Frame) -> Result<(), String> {
        Ok(())
    }
    fn size(&self) -> (u32, u32) {
        SIZE
    }
    fn device(&self) -> String {
        "/dev/video9".into()
    }
}

/// Only the main camera opens.
struct Cameras;

impl Outputs for Cameras {
    fn slots(&self) -> Vec<Slot> {
        (0..4).map(|slot| Slot { slot, name: format!("Telescope {slot}"), device: String::new() }).collect()
    }
    fn set_up(&self, _action: SetupAction, _progress: &dyn Fn(&str)) -> SetupResult {
        SetupResult { ok: true, message: String::new(), command: None }
    }
    fn formats(&self) -> Vec<PixelFormat> {
        vec![PixelFormat::I420]
    }
    fn open(&self, slot: u32, _width: u32, _height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        if slot == 0 { Ok(Box::new(Cam)) } else { Err("not this one".into()) }
    }
}

struct NoLink;

impl Link for NoLink {
    fn new_identity(&self) -> (String, String) {
        Default::default()
    }
    fn resolve(&self, _phone: &PairedPhone, _prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
        unimplemented!()
    }
    fn pair(&self, _id: &str, _name: &str) -> Result<Arc<dyn PairingOffer>, String> {
        Err("no".into())
    }
}

/// What plugins hear, and a way in to what they can ask the host.
struct Probe(Arc<Mutex<Vec<Value>>>);

impl Plugin for Probe {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "probe".into(), title: "Probe".into(), ..Default::default() }
    }
    fn on_event(&mut self, _host: &mut dyn Host, event: &Event) {
        if let Event::CameraOpened { source_id, slot, width, height } = event {
            self.0.lock().unwrap().push(json!([source_id, slot, width, height]));
        }
    }
}

fn pump_until(w: &Rc<window::Window>, what: &str, done: impl Fn(&Rc<window::Window>) -> bool) {
    let started = Instant::now();
    while !done(w) {
        offscreen::pump();
        w.process();
        assert!(started.elapsed() < Duration::from_secs(10), "{what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn ask<R>(w: &Rc<window::Window>, f: impl FnOnce(&mut dyn Host) -> R) -> R {
    let mut st = w.state.borrow_mut();
    let i = st.index("probe").unwrap();
    st.call(i, |_, host| f(host)).unwrap()
}

fn frames(w: &Rc<window::Window>) -> Vec<(String, String)> {
    ask(w, |host| host.frames().stream_frames().into_iter().map(|f| (f.source_id, f.name)).collect())
}

#[cfg(target_os = "linux")] // the fixed canvas; on Windows a camera opens at the stream's size
#[test]
fn the_window_shares_its_streams_says_which_goes_to_each_camera_and_when_it_opens() {
    assert!(offscreen::install());
    let dir = telescope_core::testing::scratch_dir("stream-window");
    let store = telescope_media::frames::Frames::new();
    let services = Arc::new(Services {
        link: Arc::new(NoLink), media: Arc::new(Fed { real: telescope_media::FfmpegMedia, store }), outputs: Arc::new(Cameras), audio: None,
        notifier: None, autostart: None, updater: None, tray: None, plugins: Box::new(Vec::new), vision: None, plugin_problems: Default::default(), plugin_admin: None,
    });
    let opened = Arc::new(Mutex::new(Vec::new()));
    let heard = opened.clone();
    let w = window::build(ConfigFile::at(dir.join(CONFIG_FILENAME)), move |_| vec![Box::new(Probe(heard)) as Box<dyn Plugin>], Some(services), None).unwrap();
    w.add_plugin_source(Arc::new(Source("test:a")));
    w.add_plugin_source(Arc::new(Source("test:b")));

    // test_the_window_shares_its_streams_as_they_start_change_and_stop
    w.stream_plugin_source("test:a");
    pump_until(&w, "the first stream's picture", |w| !frames(w).is_empty());
    assert_eq!(frames(&w), [("test:a".to_string(), "Camera test:a".to_string())]);
    assert_eq!(ask(&w, |host| host.frames().stream_frame(Some("test:a")).unwrap().frame.data[0]), 9);

    // test_the_window_says_which_stream_goes_to_each_camera_and_when_it_opens
    pump_until(&w, "the camera opening", |_| !opened.lock().unwrap().is_empty());
    assert_eq!(*opened.lock().unwrap(), [json!(["test:a", 0, 1920, 1080])]);
    w.stream_plugin_source("test:b");
    pump_until(&w, "the second stream's picture", |w| frames(w).len() == 2);
    assert_eq!(ask(&w, |host| host.stream_on_camera(0)), Some(("test:a".to_string(), Some(1920), Some(1080))));
    assert_eq!(ask(&w, |host| host.stream_on_camera(1)), Some(("test:b".to_string(), None, None)));
    assert_eq!(ask(&w, |host| host.stream_on_camera(2)), None);

    // The camera went off: its session stays, without a worker, and it's no longer shared
    w.with_flow(|flow, _| flow.sessions.find(Some("test:b")).unwrap().state.borrow_mut().session.as_mut().unwrap().worker = None);
    pump_until(&w, "the stream that went off leaving the list", |w| frames(w).len() == 1);
    assert_eq!(ask(&w, |host| host.frames().stream_frame(Some("test:b")).map(|f| f.source_id)), None);

    w.with_flow(|flow, host| flow.stop_stream(host, Some("test:a")));
    pump_until(&w, "the stream that stopped leaving the list", |w| frames(w).is_empty());
    assert_eq!(ask(&w, |host| host.frames().stream_frame(Some("test:a")).map(|f| f.source_id)), None);
    w.with_flow(|flow, host| flow.stop_stream(host, Some("test:b")));
}
