//! On Air, QR codes and Photos in the real shell state, not a stand-in for it: the settings file they save to and are loaded from, the
//! events and posts the shell delivers, the timers they ask for and what the window is left to carry out. (A stand-in host once hid a
//! bug, which is why these are here.)

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::config::ConfigFile;
use telescope_core::event::Event;
use telescope_core::frame::{CpuStep, Frame, FrameContext, FrameStep, PixelFormat};
use telescope_core::link::{Link, PairingOffer, PhoneSession, PhoneStills, RoutePrefs, Unreachable};
use telescope_core::source::PairedPhone;
use telescope_core::testing::{scratch_dir, FakeWeb};
use telescope_core::ui::UiEvent;
use telescope_effects::on_air_plugin::OnAirPlugin;
use telescope_effects::photos::PhotosPlugin;
use telescope_effects::qr_reader::QrReaderPlugin;
use telescope_link::stills::Stills;
use telescope_shell::shell::{Effect, State};

fn state_with(config: Value) -> (State, ConfigFile) {
    let file = ConfigFile::at(scratch_dir("cards2").join("telescope_config.json"));
    let mut config = config;
    file.save(&mut config);
    (State::new(ConfigFile::at(file.path())), file)
}

fn props_of(state: &State, plugin: &str, ui: &str) -> Value {
    let mut all = serde_json::Map::new();
    for effect in &state.effects {
        if let Effect::Props { plugin: p, ui: u, props } = effect {
            if p == plugin && u == ui {
                all.extend(props.as_object().unwrap().clone());
            }
        }
    }
    Value::Object(all)
}

fn ui(state: &mut State, plugin: &str, section: &str, callback: &str, args: Vec<Value>) {
    state.ui_event(plugin, UiEvent { ui: section.into(), callback: callback.into(), args });
}

fn timer(state: &mut State, plugin: &str, token: &str) {
    state.deliver_to(plugin, &Event::TimerFired { owner: plugin.into(), token: token.into() });
}

// ── QR codes ─────────────────────────────────────────────────────────────────

struct NoContext;

impl FrameContext for NoContext {
    fn source_id(&self) -> &str {
        ""
    }
    fn person_mask(&self, _frame: &Frame) -> Option<Arc<Vec<u8>>> {
        None
    }
    fn colour_preview(&self, frame: &Frame) -> Frame {
        frame.clone()
    }
}

/// A frame with a code in it, as the decoder makes them.
fn frame_with_code(text: &str) -> Frame {
    let (cells, size) = telescope_core::qr::encode(text);
    let (w, h, scale) = (640usize, 360usize, 6usize);
    let mut luma = vec![90u8; w * h];
    for row in 0..(size + 8) * scale {
        for col in 0..(size + 8) * scale {
            let (r, c) = ((row / scale) as i64 - 4, (col / scale) as i64 - 4);
            let dark = r >= 0 && c >= 0 && (r as usize) < size && (c as usize) < size && cells.contains(&(r as usize * size + c as usize));
            luma[(40 + row) * w + 300 + col] = if dark { 16 } else { 235 };
        }
    }
    luma.extend(vec![128; 2 * (w / 2) * (h / 2)]);
    Frame::new(w as u32, h as u32, PixelFormat::I420, luma)
}

#[test]
fn a_code_held_up_to_the_camera_reaches_the_clipboard_and_a_notification() {
    let (mut state, file) = state_with(json!({"plugin_configs": {"qr_reader": {"on": true}}}));
    state.register(Box::new(QrReaderPlugin::new())).unwrap();
    state.apply_saved_config();
    state.deliver_events();
    assert!(state.slots[0].info.frame_stage == telescope_core::FrameStage::Reading);

    // The frame step the shell gives the stream looks at the picture on a thread of its own and posts what it finds
    let steps = state.pipeline_steps();
    let mut step = steps.into_iter().find(|s| s.owner == "qr_reader").map(|s| s.step).expect("a frame step");
    let FrameStep::Cpu(step) = &mut step else { panic!("a Rust step") };
    let frame = frame_with_code("WIFI:T:WPA;S:Telescope;P:hunter22;;");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !state.effects.iter().any(|e| matches!(e, Effect::CopyText(_))) {
        assert!(Instant::now() < deadline, "no copy: {:?}", state.effects);
        let mut copy = frame.clone();
        step.process(&mut copy, &NoContext);
        std::thread::sleep(Duration::from_millis(40));
        state.deliver_events();
    }
    assert!(state.effects.contains(&Effect::CopyText("hunter22".into())));
    assert!(state.effects.iter().any(|e| matches!(e, Effect::Schedule { plugin, token, .. } if plugin == "qr_reader" && token == "check")));

    // The window put it on the clipboard; the check a moment later reads it back
    state.clipboard = Some(Box::new(|| Some("hunter22".to_string())));
    timer(&mut state, "qr_reader", "check");
    assert!(state.effects.iter().any(|e| matches!(e, Effect::Notify { title, body, .. } if title == "QR code copied" && body == "Copied the password for Telescope")));
    assert_eq!(props_of(&state, "qr_reader", "card")["status-kind"], "status_ok");

    // What it saves is what it loads
    assert!(state.save_now());
    assert_eq!(file.load()["plugin_configs"]["qr_reader"], json!({"on": true, "notify": true}));
}

#[test]
fn a_clipboard_that_ignored_the_copy_says_to_click_copy() {
    let (mut state, _) = state_with(json!({"plugin_configs": {"qr_reader": {"on": true, "notify": true}}}));
    state.register(Box::new(QrReaderPlugin::new())).unwrap();
    state.apply_saved_config();
    for _ in 0..3 {
        state.bus.emit(Event::Posted { owner: "qr_reader".into(), token: "found".into(), data: json!({"text": "https://example.com", "corners": [[0.4, 0.2], [0.6, 0.2], [0.6, 0.55], [0.4, 0.55]]}) });
    }
    // Posts come from a plugin's threads, so they go through the shell's queue like the real ones
    for _ in 0..3 {
        state.posts.push("qr_reader", "found", json!({"text": "https://example.com", "corners": [[0.4, 0.2], [0.6, 0.2], [0.6, 0.55], [0.4, 0.55]]}));
        state.deliver_events();
    }
    assert!(state.effects.contains(&Effect::CopyText("https://example.com".into())));
    state.clipboard = Some(Box::new(|| Some("old".to_string())));
    timer(&mut state, "qr_reader", "check");
    assert!(state.effects.iter().any(|e| matches!(e, Effect::Notify { title, .. } if title == "QR code found")));
    assert_eq!(props_of(&state, "qr_reader", "card")["status-kind"], "status_warn");
}

// ── On Air ───────────────────────────────────────────────────────────────────

const TOKEN: &str = "abcdefghijklmnop_-ABCDEF";

struct OnAirRig {
    state: State,
    file: ConfigFile,
    web: Arc<FakeWeb>,
    now: Arc<Mutex<f64>>,
    sent: Arc<Mutex<Vec<String>>>,
}

fn on_air_rig(config: Value) -> OnAirRig {
    let (mut state, file) = state_with(json!({"plugin_configs": {"on_air": config}}));
    let web = Arc::new(FakeWeb::default());
    *web.addresses.lock().unwrap() = vec!["192.168.1.20".into()];
    let (now, sent) = (Arc::new(Mutex::new(100.0)), Arc::new(Mutex::new(Vec::new())));
    let (clock, record) = (now.clone(), sent.clone());
    let plugin = OnAirPlugin::for_test(web.clone(), None, scratch_dir("on-air"), move || *clock.lock().unwrap(), move |request| {
        record.lock().unwrap().push(request.url.clone());
        String::new()
    });
    state.register(Box::new(plugin)).unwrap();
    state.apply_saved_config();
    state.deliver_events();
    OnAirRig { state, file, web, now, sent }
}

impl OnAirRig {
    fn later(&mut self, seconds: f64) {
        *self.now.lock().unwrap() += seconds;
        timer(&mut self.state, "on_air", "settle");
    }

    fn token(&self) -> String {
        self.state.plugin_config("on_air").unwrap()["token"].as_str().unwrap().to_string()
    }
}

#[test]
fn the_sign_goes_on_and_the_header_and_the_page_follow() {
    let mut rig = on_air_rig(json!({"token": TOKEN, "webhook_url": "http://hub.local/hook"}));
    assert_eq!(rig.token(), TOKEN);
    assert_eq!(rig.web.served.lock().unwrap().len(), 1, "the sign page is up once the plugins are ready");
    rig.state.bus.emit(Event::CameraWatched { watched: true });
    rig.state.deliver_events();
    assert!(rig.state.effects.iter().any(|e| matches!(e, Effect::Schedule { plugin, token, .. } if plugin == "on_air" && token == "settle")));
    rig.later(2.0);
    rig.state.deliver_events();
    assert_eq!(props_of(&rig.state, "on_air", "header")["shown"], true);
    assert_eq!(props_of(&rig.state, "on_air", "on_air")["state-text"], "On air");
    // The webhook went out on its thread and its answer came back through the shell's posts
    let deadline = Instant::now() + Duration::from_secs(5);
    while rig.sent.lock().unwrap().len() < 2 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(*rig.sent.lock().unwrap(), ["http://hub.local/hook", "http://hub.local/hook"]);
    std::thread::sleep(Duration::from_millis(100));
    rig.state.deliver_events();
    assert!(rig.state.banners.keys().is_empty(), "it went through: no banner");
}

#[test]
fn the_link_is_saved_and_comes_back_after_a_restart() {
    let mut rig = on_air_rig(json!({}));
    let token = rig.token();
    assert_eq!(token.len(), 24, "a link was made");
    // A link made fresh is saved straight away: setup's has to survive a restart
    assert!(rig.state.effects.contains(&Effect::SaveSoon));
    assert!(rig.state.save_now());
    assert_eq!(rig.file.load()["plugin_configs"]["on_air"]["token"], token.as_str());

    let again = {
        let file = ConfigFile::at(rig.file.path());
        let mut state = State::new(file);
        let plugin = OnAirPlugin::for_test(rig.web.clone(), None, scratch_dir("on-air"), || 0.0, |_| String::new());
        state.register(Box::new(plugin)).unwrap();
        state.apply_saved_config();
        state
    };
    assert_eq!(again.plugin_config("on_air").unwrap()["token"], token.as_str());
    let section = props_of(&again, "on_air", "on_air");
    let _ = section;
    // Its sign page is on the same link
    assert_eq!(rig.web.served.lock().unwrap().len(), 2);
}

#[test]
fn a_new_link_and_the_settings_it_keeps_are_saved() {
    let mut rig = on_air_rig(json!({"token": TOKEN}));
    ui(&mut rig.state, "on_air", "on_air", "new-link-clicked", vec![]);
    assert_ne!(rig.token(), TOKEN);
    ui(&mut rig.state, "on_air", "on_air", "trigger-picked", vec![json!(2)]);
    ui(&mut rig.state, "on_air", "on_air", "url-edited", vec![json!("http://hub.local/x")]);
    ui(&mut rig.state, "on_air", "on_air", "method-picked", vec![json!(1)]);
    ui(&mut rig.state, "on_air", "on_air", "on-edited", vec![json!("on=1")]);
    assert!(rig.state.save_now());
    let saved = rig.file.load()["plugin_configs"]["on_air"].clone();
    assert_eq!((&saved["trigger"], &saved["webhook_url"], &saved["webhook_method"], &saved["webhook_on"]), (&json!("mic"), &json!("http://hub.local/x"), &json!("GET"), &json!("on=1")));
    assert_eq!(saved["token"], rig.token().as_str());
}

#[test]
fn opening_settings_tells_the_section_so_and_the_link_follows_the_network() {
    let mut rig = on_air_rig(json!({"token": TOKEN}));
    *rig.web.addresses.lock().unwrap() = vec!["10.0.0.7".into()];
    ui(&mut rig.state, "on_air", "on_air", "shown", vec![]);
    assert_eq!(props_of(&rig.state, "on_air", "on_air")["url-text"], format!("https://10.0.0.7:8768/#{TOKEN}").as_str());
    assert_eq!(rig.state.plugin_config("on_air").unwrap()["address"], "10.0.0.7");
}

// ── Photos ───────────────────────────────────────────────────────────────────

/// A link that only has the photos service, which is the real one.
struct StillsLink(Arc<Stills>);

impl Link for StillsLink {
    fn new_identity(&self) -> (String, String) {
        Default::default()
    }
    fn resolve(&self, _phone: &PairedPhone, _prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
        unimplemented!()
    }
    fn pair(&self, _id: &str, _name: &str) -> Result<Arc<dyn PairingOffer>, String> {
        Err("no".into())
    }
    fn stills(&self) -> Option<Arc<dyn PhoneStills>> {
        Some(self.0.clone())
    }
}

fn phone_state() -> Value {
    json!({"stills": 1, "cameras": [{"id": "0", "current": true, "stills": {
        "formats": ["jpeg", "raw"], "width": 4000, "height": 3000, "burstMax": 10, "noiseReductionHq": true}}]})
}

#[test]
fn a_photo_is_taken_through_the_shell_and_every_plugin_hears_what_became_of_it() {
    let (mut state, file) = state_with(json!({"plugin_configs": {"photos": {"folder": "/tmp/shots", "format": "raw+jpeg", "quality": 80}}}));
    let stills = Arc::new(Stills::new());
    state.link = Some(Arc::new(StillsLink(stills.clone())));
    state.register(Box::new(PhotosPlugin::new())).unwrap();
    state.apply_saved_config();
    state.bus.emit(Event::PhonesChanged { count: 1 });
    state.deliver_events();
    assert_eq!(props_of(&state, "photos", "card")["take-enabled"], false, "nothing streams yet");
    assert!(props_of(&state, "photos", "card")["info"].as_str().unwrap().contains("Start streaming"));

    // A stream is up and its phone has said what its lens takes
    state.streams.streaming = true;
    state.streams.source_id = Some("p1".into());
    state.streams.live = vec!["p1".into()];
    stills.note_state(Some("p1"), &phone_state());
    state.bus.emit(Event::PhoneStateUpdated { state: phone_state() });
    state.deliver_events();
    let card = props_of(&state, "photos", "card");
    assert_eq!((&card["take-enabled"], &card["format-shown"]), (&json!(true), &json!(true)));
    assert_eq!(card["format-items"], json!(["JPEG", "RAW (DNG)", "RAW and JPEG"]));

    state.effects.clear();
    ui(&mut state, "photos", "card", "take-clicked", vec![]);
    let taken = state.effects.iter().find_map(|e| match e {
        Effect::TakeStill { plugin, request, source, options, folder } => Some((plugin.clone(), *request, source.clone(), options.clone(), folder.clone())),
        _ => None,
    }).expect("the window is asked to take it");
    assert_eq!((taken.0.as_str(), taken.2.as_str(), taken.4.as_str()), ("photos", "p1", "/tmp/shots"));
    assert_eq!(taken.3, json!({"format": "jpeg", "rotation": 0, "raw": 1, "quality": 80, "nr_hq": 1}));
    assert_eq!(props_of(&state, "photos", "card")["status"], "Taking the photo…");

    // The photo's thread reports back through the posts, to the plugin that asked
    state.posts.push_event("photos", Event::RequestProgress { request: taken.1, data: json!({"state": "capturing", "shots": 3, "taken": 1}) });
    state.deliver_events();
    assert_eq!(props_of(&state, "photos", "card")["status"], "Photo 2 of 3");
    state.posts.push_event("photos", Event::RequestDone { request: taken.1, ok: true, message: String::new(), data: json!({"ok": true, "files": ["/tmp/shots/IMG_1.jpg", "/tmp/shots/IMG_1.dng"]}) });
    state.deliver_events();
    let card = props_of(&state, "photos", "card");
    assert_eq!((card["status"].as_str(), card["status-kind"].as_str()), (Some("Saved IMG_1.jpg and 1 more"), Some("status_ok")));
    // Nobody had the window in front: a notification says so
    assert!(state.effects.iter().any(|e| matches!(e, Effect::Notify { title, .. } if title == "Photo saved")));
    assert!(state.stills.is_empty(), "the shell is done keeping track of it");

    // What the card keeps is saved as the file has always had it
    assert!(state.save_now());
    let saved = file.load()["plugin_configs"]["photos"].clone();
    assert_eq!((&saved["folder"], &saved["format"], &saved["quality"]), (&json!("/tmp/shots"), &json!("raw+jpeg"), &json!(80)));
}

#[test]
fn a_photo_the_stream_ends_under_is_let_go() {
    let (mut state, _) = state_with(json!({}));
    let alive = Arc::new(std::sync::atomic::AtomicBool::new(true));
    state.still_alive.push(("p1".into(), alive.clone()));
    state.still_alive.push(("p2".into(), Arc::new(std::sync::atomic::AtomicBool::new(true))));
    state.let_go_of_photos(Some("p1"));
    assert!(!alive.load(std::sync::atomic::Ordering::SeqCst) && state.still_alive.len() == 1);
    state.bus.emit(Event::StreamStopped);
    state.deliver_events();
    assert!(state.still_alive.is_empty());
}
