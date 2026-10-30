//! Startup and the Wait screen through the real shell state: what they ask of the window for (the tray, timers, stopping a
//! stream, a frame step in the pipeline), what they save, and how they follow what the other plugins say on the bus.

use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use telescope_core::config::ConfigFile;
use telescope_core::event::Event;
use telescope_core::frame::{Frame, FrameStage, FrameStep, PixelFormat};
use telescope_core::output::{Held, Outputs, VideoOutput, WaitScreen};
use telescope_core::plugin::Plugin;
use telescope_core::system::Autostart;
use telescope_core::testing::scratch_dir;
use telescope_core::ui::UiEvent;
use telescope_outputs::wait_screen_plugin::WaitScreenPlugin;
use telescope_shell::shell::{Effect, State};
use telescope_system::startup::StartupPlugin;

struct SignIn;

impl Autostart for SignIn {
    fn enabled(&self) -> bool {
        false
    }
    fn set_enabled(&self, _on: bool, _minimized: bool) -> Result<(), String> {
        Ok(())
    }
}

/// A virtual camera that isn't there: its wait screens remember what they were told, and the watch what to call.
#[derive(Default)]
struct Cameras {
    shown: Arc<Mutex<Vec<(u32, (u32, u32), Option<String>, bool)>>>,
    on_change: Mutex<Option<Box<dyn Fn(bool) + Send + Sync>>>,
}

struct Screen(u32, Arc<Mutex<Vec<(u32, (u32, u32), Option<String>, bool)>>>);

impl Held for Screen {
    fn start(&self) {}
    fn stop(&self) {}
    fn running(&self) -> bool {
        true
    }
}

impl WaitScreen for Screen {
    fn show(&self, width: u32, height: u32, path: Option<&str>, mirror: bool) {
        self.1.lock().unwrap().push((self.0, (width, height), path.map(String::from), mirror));
    }
    fn set_watched(&self, _watched: bool) {}
}

struct Idle;

impl Held for Idle {
    fn start(&self) {}
    fn stop(&self) {}
    fn running(&self) -> bool {
        true
    }
}

impl Outputs for Cameras {
    fn slots(&self) -> Vec<telescope_core::output::Slot> {
        Vec::new()
    }
    fn formats(&self) -> Vec<PixelFormat> {
        vec![PixelFormat::I420]
    }
    fn open(&self, _slot: u32, _width: u32, _height: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        Err("no camera".into())
    }
    fn wait_screen(&self, slot: u32) -> Option<Box<dyn WaitScreen>> {
        Some(Box::new(Screen(slot, self.shown.clone())))
    }
    fn watch(&self, on_change: Box<dyn Fn(bool) + Send + Sync>) -> Option<Box<dyn Held>> {
        *self.on_change.lock().unwrap() = Some(on_change);
        Some(Box::new(Idle))
    }
}

fn config_with(plugin_configs: Value) -> (std::path::PathBuf, ConfigFile) {
    let path = scratch_dir("startup-wait-state").join("telescope_config.json");
    let file = ConfigFile::at(&path);
    file.save(&mut json!({"plugin_configs": plugin_configs}));
    (path, file)
}

fn state(file: &ConfigFile, plugins: Vec<Box<dyn Plugin>>) -> State {
    let mut s = State::new(ConfigFile::at(file.path()));
    for p in telescope_shell::plugins::in_order(plugins) {
        s.register(p).unwrap();
    }
    s.apply_saved_config();
    s
}

fn pushed(s: &State, wanted: &Effect) -> bool {
    s.effects.contains(wanted)
}

#[test]
fn saved_startup_settings_keep_the_window_in_the_tray_and_start_a_stream_for_the_phone() {
    let (_, file) = config_with(json!({"startup": {"start_on": "ready", "stop_idle": "any", "stop_delay_s": 30, "notify_stop": true}}));
    let mut s = state(&file, vec![Box::new(StartupPlugin::new(Arc::new(SignIn)))]);
    // Loaded before the window is up, and told to the window once it is
    assert_eq!(s.plugin_config("startup").unwrap()["stop_delay_s"], 30);
    assert!(s.keep_in_tray && pushed(&s, &Effect::KeepInTray(true)));
    s.deliver_events();

    // The phone is ready: it starts without asking, once
    s.effects.clear();
    s.bus.emit(Event::PhoneReady { phone_id: "p1".into(), ready: true });
    s.bus.emit(Event::PhoneReady { phone_id: "p1".into(), ready: true });
    s.deliver_events();
    assert_eq!(s.effects.iter().filter(|e| **e == Effect::Start { interactive: false }).count(), 1);

    // It's up and no app reads the camera: the wait starts, and when it's over the stream stops with a notice
    s.effects.clear();
    s.streams.streaming = true;
    s.bus.emit(Event::StreamStarted { url: "https://phone/v1/video".into() });
    s.bus.emit(Event::CameraWatched { watched: false });
    s.deliver_events();
    assert!(pushed(&s, &Effect::Schedule { plugin: "startup".into(), token: "idle-stop".into(), ms: 30_000 }), "{:?}", s.effects);
    s.effects.clear();
    s.deliver_to("startup", &Event::TimerFired { owner: "startup".into(), token: "idle-stop".into() });
    assert!(pushed(&s, &Effect::Stop(None)));
    assert!(s.effects.iter().any(|e| matches!(e, Effect::Notify { title, body, urgent: false }
        if title == "Telescope stopped streaming" && body == "No app used the camera for 30 s.")));
    // What it saved is what it loaded
    assert!(s.save_now());
    let saved = file.load();
    assert_eq!(saved["plugin_configs"]["startup"], json!({"start_on": "ready", "stop_idle": "any", "stop_delay_s": 30, "notify_stop": true}));
}

#[test]
fn old_startup_checkboxes_are_saved_as_the_new_choices() {
    let (_, file) = config_with(json!({"startup": {"watch_stream": true, "auto_stream": true}}));
    let mut s = state(&file, vec![Box::new(StartupPlugin::new(Arc::new(SignIn)))]);
    assert!(s.save_now());
    assert_eq!(file.load()["plugin_configs"]["startup"], json!({"start_on": "watched", "stop_idle": "own", "stop_delay_s": 15, "notify_stop": true}));
}

#[test]
fn startup_sections_are_listed_on_general_and_streaming() {
    let (_, file) = config_with(json!({}));
    let mut s = state(&file, vec![Box::new(StartupPlugin::new(Arc::new(SignIn)))]);
    let i = s.index("startup").unwrap();
    let sections = s.call(i, |p, _| p.settings_sections()).unwrap();
    assert_eq!(sections.iter().map(|x| (x.tab.as_str(), x.id.as_str())).collect::<Vec<_>>(), [("general", "sign-in"), ("streaming", "automatic")]);
    // Picking from the page changes the plugin and keeps the page up to date
    s.effects.clear();
    s.ui_event("startup", UiEvent { ui: "automatic".into(), callback: "stop-picked".into(), args: vec![json!(2)] });
    assert_eq!(s.plugin_config("startup").unwrap()["stop_idle"], "any");
    assert!(s.effects.iter().any(|e| matches!(e, Effect::Props { ui, props, .. } if ui == "automatic" && props["stops"] == json!(true))));
    assert!(pushed(&s, &Effect::SaveSoon));
}

#[test]
fn the_wait_screen_follows_the_settings_the_phone_and_its_cameras() {
    let (_, file) = config_with(json!({"wait_screen": {"image": null, "mirror": true, "last_size": [640, 480]}}));
    let cameras = Arc::new(Cameras::default());
    let mut s = state(&file, vec![Box::new(WaitScreenPlugin::new(cameras.clone()))]);
    // Shown once every plugin's settings are in, at the size the last stream had, mirrored as saved
    assert_eq!(*cameras.shown.lock().unwrap(), [(0, (640, 480), None, true)]);

    // A stream opening the camera at another size is remembered
    s.bus.emit(Event::VcamOpened { width: 1280, height: 720 });
    s.deliver_events();
    assert!(pushed(&s, &Effect::SaveSoon));
    assert!(s.save_now());
    assert_eq!(file.load()["plugin_configs"]["wait_screen"], json!({"image": null, "mirror": true, "last_size": [1280, 720]}));

    // An app opens the virtual camera, which the watch's thread says: everyone else hears it on the bus
    (cameras.on_change.lock().unwrap().as_ref().expect("a watch"))(true);
    s.deliver_events();
    assert!(s.diagnostics().contains(&("An app is reading the camera".into(), "yes".into())));

    // Cameras nothing streams to get one too
    s.bus.emit(Event::IdleOutputs { slots: vec![1] });
    s.deliver_events();
    assert!(cameras.shown.lock().unwrap().iter().any(|c| c.0 == 1));
}

#[test]
fn asked_to_stand_in_for_the_stream_the_wait_screen_joins_the_pipeline_at_its_stage() {
    let (_, file) = config_with(json!({}));
    let cameras = Arc::new(Cameras::default());
    let mut s = state(&file, vec![Box::new(WaitScreenPlugin::new(cameras))]);
    assert!(s.pipeline_steps(None).iter().all(|step| step.owner != "wait_screen"));
    s.effects.clear();
    s.bus.emit(Event::CoverRequested { owner: "Privacy blur".into(), on: true });
    s.deliver_events();
    // The asker hears it's in place, and the window is told to rebuild the steps
    assert!(pushed(&s, &Effect::FrameStep("wait_screen".into())));
    let mut steps = s.pipeline_steps(None);
    let step = steps.iter_mut().find(|step| step.owner == "wait_screen").expect("its step");
    assert_eq!(step.stage, FrameStage::Cover);
    let FrameStep::Cpu(cpu) = &mut step.step else { panic!("a CPU step") };
    struct Ctx;
    impl telescope_core::frame::FrameContext for Ctx {
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
    // The stream's picture is replaced at its own size (black until the screen is made on its thread)
    let mut frame = Frame::new(64, 36, PixelFormat::Rgba, vec![99; 64 * 36 * 4]);
    frame.number = 7;
    cpu.process(&mut frame, &Ctx);
    assert_eq!((frame.width, frame.height, frame.number), (64, 36, 7));
    assert_ne!(frame.data[0], 99);
    s.bus.emit(Event::CoverRequested { owner: "Privacy blur".into(), on: false });
    s.deliver_events();
    assert!(s.pipeline_steps(None).iter().all(|step| step.owner != "wait_screen"));
}

#[test]
fn a_picked_image_is_kept_and_saved_and_the_page_shows_it() {
    let (path, file) = config_with(json!({}));
    let cameras = Arc::new(Cameras::default());
    let folder = path.parent().unwrap().to_path_buf();
    let mut s = state(&file, vec![Box::new(WaitScreenPlugin::new(cameras.clone()).keeping_in(move || folder.clone()))]);
    let source = scratch_dir("startup-wait-state-source").join("logo.png");
    std::fs::write(&source, b"png").unwrap();
    // The page's Choose image… asks for a file, and the answer comes back as the request's event
    s.ui_event("wait_screen", UiEvent { ui: "wait-screen".into(), callback: "choose-clicked".into(), args: vec![] });
    let request = s.effects.iter().find_map(|e| match e { Effect::PickFile { request, .. } => Some(*request), _ => None });
    // The shell only shows the dialog on Linux; elsewhere nothing is asked and nothing is waiting
    if let Some(request) = request {
        s.deliver_to("wait_screen", &Event::RequestDone { request, ok: true, message: String::new(), data: json!({"path": source.to_str(), "filter": ""}) });
        let saved = s.plugin_config("wait_screen").unwrap();
        let kept = saved["image"].as_str().expect("a kept copy");
        assert!(std::path::Path::new(kept).file_name().unwrap().to_string_lossy().starts_with("wait_screen-"));
        assert_eq!(std::fs::read(kept).unwrap(), b"png");
        assert!(cameras.shown.lock().unwrap().iter().any(|c| c.2.as_deref() == Some(kept)));
        assert!(s.save_now());
        assert_eq!(file.load()["plugin_configs"]["wait_screen"]["image"], kept);
    }
}
