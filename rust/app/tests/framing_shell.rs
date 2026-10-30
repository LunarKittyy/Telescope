//! Auto framing and Expose for my face in the real shell state, not a stand-in for it: the settings they load, the bus that delays what they say, the view they steer and, with a vision pack (TELESCOPE_VISION_PACK), the real models.

use serde_json::{json, Value};
use std::io::Read;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::config::ConfigFile;
use telescope_core::event::Event;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::host::Host;
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::pose::Person;
use telescope_core::testing::scratch_dir;
use telescope_core::ui::UiEvent;
use telescope_core::vision::{Upright, VisionService};
use telescope_effects::face_exposure::FaceExposurePlugin;
use telescope_effects::framing::{fit, target_box, FramingPlugin, MAX_ZOOM};
use telescope_effects::transforms::TransformsPlugin;
use telescope_shell::shell::{Effect, State};
use telescope_vision::service::{Backend, Find};
use telescope_vision::VisionHost;

type Clock = Arc<Mutex<f64>>;

fn state_with(config: Value) -> State {
    let file = ConfigFile::at(scratch_dir("framing").join("telescope_config.json"));
    let mut config = config;
    file.save(&mut config);
    State::new(ConfigFile::at(file.path()))
}

fn framing(clock: &Clock) -> Box<FramingPlugin> {
    let time = clock.clone();
    Box::new(FramingPlugin::with_clock(move || *time.lock().unwrap()))
}

fn face(clock: &Clock) -> Box<FaceExposurePlugin> {
    let time = clock.clone();
    Box::new(FaceExposurePlugin::with_clock(move || *time.lock().unwrap()))
}

/// Writes down the points that reach the phone, as the Camera card would hear them.
struct Camera(Arc<Mutex<Vec<(f32, f32)>>>);

impl Plugin for Camera {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "camera_control".into(), ..Default::default() }
    }
    fn on_event(&mut self, _host: &mut dyn Host, event: &Event) {
        if let Event::FocusPoint { x, y } = event {
            self.0.lock().unwrap().push((*x, *y));
        }
    }
}

fn config_of(state: &mut State, plugin: &str) -> Value {
    state.plugin_config(plugin).expect("a plugin")
}

fn tick(state: &mut State) {
    state.deliver_to("framing", &Event::TimerFired { owner: "framing".into(), token: "tick".into() });
    state.deliver_events();
}

fn diagnosis(state: &mut State, plugin: &str) -> String {
    let said = Arc::new(Mutex::new(String::new()));
    let out = said.clone();
    state.each(|p, _| {
        if p.info().name == plugin {
            *out.lock().unwrap() = p.diagnostics()[0].1.clone();
        }
    });
    let text = said.lock().unwrap().clone();
    text
}

#[test]
fn saved_settings_put_framing_on_and_it_takes_the_view() {
    let clock: Clock = Arc::new(Mutex::new(10.0));
    let mut s = state_with(json!({"selected_device": "A", "devices": {"A": {"plugin_configs": {"framing": {"on": true, "speed": 8}}}}}));
    s.register(framing(&clock)).unwrap();
    s.apply_saved_config();
    s.deliver_events();
    assert_eq!(config_of(&mut s, "framing")["speed"], 8);
    assert_eq!(diagnosis(&mut s, "framing"), "on");
    assert!(s.effects.iter().any(|e| matches!(e, Effect::Schedule { plugin, token, ms: 33 } if plugin == "framing" && token == "tick")));
}

// test_auto_framing_on_one_camera_doesnt_zoom_the_next_one_out, on the device change the settings go through
#[test]
fn auto_framing_on_one_camera_doesnt_zoom_the_next_one_out() {
    let clock: Clock = Arc::new(Mutex::new(10.0));
    let mut s = state_with(json!({"selected_device": "A", "devices": {
        "A": {"plugin_configs": {"framing": {"on": true}}},
        "B": {"plugin_configs": {"transforms": {"zoom": 2.0}}},
    }}));
    s.register(Box::new(TransformsPlugin::new())).unwrap();
    s.register(framing(&clock)).unwrap();
    s.apply_saved_config();
    s.deliver_events();
    // Framing someone on the face camera
    let person = json!({"points": {"left_shoulder": [0.55, 0.4, 0.99], "right_shoulder": [0.45, 0.4, 0.99], "nose": [0.5, 0.3, 0.99]}});
    for _ in 0..30 {
        *clock.lock().unwrap() += 1.0 / 30.0;
        s.bus.emit(Event::PeopleSeen { source_id: "A".into(), people: vec![person.clone()] });
        s.deliver_events();
        tick(&mut s);
    }
    assert!(s.view.zoom > 1.2, "{}", s.view.zoom);
    s.switch_device(Some("A"), Some("B"));
    s.deliver_events();
    tick(&mut s);
    assert_eq!(config_of(&mut s, "framing")["on"], false);
    assert_eq!(diagnosis(&mut s, "framing"), "off");
    assert_eq!(s.view.zoom, 2.0);
}

#[test]
fn the_point_expose_for_my_face_says_reaches_the_phone_and_doesnt_count_as_picked_by_hand() {
    let clock: Clock = Arc::new(Mutex::new(100.0));
    let points = Arc::new(Mutex::new(Vec::new()));
    let mut s = state_with(json!({"selected_device": "phone", "devices": {"phone": {"plugin_configs": {"face_exposure": {"on": true}}}}}));
    s.register(Box::new(Camera(points.clone()))).unwrap();
    s.register(face(&clock)).unwrap();
    s.apply_saved_config();
    s.streams.source_id = Some("phone".into());
    s.frame_sizes.lock().unwrap().insert("phone".into(), (1920, 1080));
    s.view.flip_h = true;
    s.bus.emit(Event::FocusPointAvailable { available: true });
    s.deliver_events();
    let track = json!({"id": 1, "box": [0.2, 0.2, 0.6, 0.9], "face": [0.25, 0.3], "seen_s": 5.0, "missing_s": 0.0});
    for _ in 0..24 {
        *clock.lock().unwrap() += 1.0 / 12.0;
        s.bus.emit(Event::PeopleTracked { source_id: "phone".into(), tracks: vec![track.clone()], count: 1 });
        s.deliver_events();
    }
    assert_eq!(diagnosis(&mut s, "face_exposure"), "following");
    let points = points.lock().unwrap();
    assert_eq!(points.len(), 1);
    // Mirrored on the way to the phone, which sees the picture before the view
    assert!((points[0].0 - 0.75).abs() < 1e-6 && (points[0].1 - 0.3).abs() < 1e-6, "{points:?}");
}

#[test]
fn the_camera_cards_point_button_counts_as_picked_by_hand() {
    let clock: Clock = Arc::new(Mutex::new(100.0));
    let mut s = state_with(json!({}));
    s.register(face(&clock)).unwrap();
    s.apply_saved_config();
    s.ui_event("face_exposure", UiEvent { ui: "card".into(), callback: "switch-toggled-by-hand".into(), args: vec![json!(true)] });
    s.bus.emit(Event::FocusModePicked { mode: "point".into() });
    s.bus.emit(Event::FocusPointPicked { x: 0.5, y: 0.5 });
    s.deliver_events();
    assert_eq!(diagnosis(&mut s, "face_exposure"), "point picked by hand");
}

fn pack() -> Option<PathBuf> {
    std::env::var_os("TELESCOPE_VISION_PACK").map(PathBuf::from)
}

fn vision_at(folder: PathBuf) -> VisionHost {
    let find: Find = Box::new(move || telescope_vision::mediapipe::MediaPipe::load(&folder).map(|b| Arc::new(b) as Arc<dyn Backend>));
    VisionHost::with(find, Some(Box::new(|| true)), scratch_dir("framing-pack"), telescope_vision::pack::PackInfo::current(), Box::new(telescope_vision::pack::web))
}

fn astronaut() -> Frame {
    let fixtures = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../crates/vision/tests/fixtures");
    let mut rgb = Vec::new();
    flate2::read::ZlibDecoder::new(std::fs::File::open(fixtures.join("astronaut.rgb.z")).unwrap()).read_to_end(&mut rgb).unwrap();
    let rgba: Vec<u8> = rgb.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect();
    Frame::new(384, 216, PixelFormat::Rgba, rgba)
}

/// Feeds the frame until the people thread answers; the events it put on the bus are still there.
fn feed_until(vision: &VisionHost, s: &State, seen: &mut dyn FnMut(&Event) -> bool) {
    let frame = astronaut();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(60) {
        vision.feed_raw("phone", &frame, Upright::default());
        std::thread::sleep(Duration::from_millis(100));
        while let Some(event) = s.bus.next() {
            if seen(&event) {
                return;
            }
        }
    }
    panic!("the models saw nobody in time");
}

#[test]
fn the_real_models_frame_the_astronaut_as_python_would() {
    let Some(folder) = pack() else {
        eprintln!("no vision pack here: set TELESCOPE_VISION_PACK to a pack folder");
        return;
    };
    let clock: Clock = Arc::new(Mutex::new(10.0));
    let mut s = state_with(json!({"selected_device": "phone", "devices": {"phone": {"plugin_configs": {"framing": {"on": true, "room": 0, "speed": 10}}}}}));
    let vision = vision_at(folder);
    vision.attach(s.bus.sender());
    let service: Arc<dyn VisionService> = Arc::new(vision.clone());
    s.vision = Some(service);
    s.register(framing(&clock)).unwrap();
    s.apply_saved_config();
    s.streams.source_id = Some("phone".into());
    s.frame_sizes.lock().unwrap().insert("phone".into(), (384, 216));
    let mut people = Vec::new();
    feed_until(&vision, &s, &mut |event| match event {
        Event::PeopleSeen { people: found, .. } if !found.is_empty() => {
            people = found.clone();
            true
        }
        _ => false,
    });
    let poses: Vec<Person> = people.iter().map(|p| Person::from_value(p).expect("a person")).collect();
    let want = fit(target_box(&poses, 384.0 / 216.0, 1.0, &[]).expect("a box"), MAX_ZOOM);
    for _ in 0..150 {
        *clock.lock().unwrap() += 1.0 / 30.0;
        s.bus.emit(Event::PeopleSeen { source_id: "phone".into(), people: people.clone() });
        s.deliver_events();
        tick(&mut s);
    }
    assert!((s.view.zoom as f64 - want.zoom).abs() < 0.02, "{} {want:?}", s.view.zoom);
    vision.shutdown();
}

#[test]
fn the_real_models_put_the_metering_point_on_the_astronauts_face() {
    let Some(folder) = pack() else { return };
    let clock: Clock = Arc::new(Mutex::new(100.0));
    let points = Arc::new(Mutex::new(Vec::new()));
    let mut s = state_with(json!({"selected_device": "phone", "devices": {"phone": {"plugin_configs": {"face_exposure": {"on": true}}}}}));
    let vision = vision_at(folder);
    vision.attach(s.bus.sender());
    let service: Arc<dyn VisionService> = Arc::new(vision.clone());
    s.vision = Some(service);
    s.register(Box::new(Camera(points.clone()))).unwrap();
    s.register(face(&clock)).unwrap();
    s.apply_saved_config();
    s.streams.source_id = Some("phone".into());
    s.frame_sizes.lock().unwrap().insert("phone".into(), (384, 216));
    s.bus.emit(Event::FocusPointAvailable { available: true });
    s.deliver_events();
    let mut tracks = Vec::new();
    feed_until(&vision, &s, &mut |event| match event {
        Event::PeopleTracked { tracks: found, .. } if found.iter().any(|t| !t["face"].is_null()) => {
            tracks = found.clone();
            true
        }
        _ => false,
    });
    let face = tracks[0]["face"].clone();
    for _ in 0..24 {
        *clock.lock().unwrap() += 1.0 / 12.0;
        s.bus.emit(Event::PeopleTracked { source_id: "phone".into(), tracks: tracks.clone(), count: 1 });
        s.deliver_events();
    }
    let points = points.lock().unwrap();
    assert_eq!(points.len(), 1);
    assert!((points[0].0 as f64 - face[0].as_f64().unwrap()).abs() < 1e-4 && (points[0].1 as f64 - face[1].as_f64().unwrap()).abs() < 1e-4, "{points:?} {face}");
    vision.shutdown();
}
