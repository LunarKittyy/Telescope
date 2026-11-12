use super::*;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use telescope_core::output::VideoOutput;
use telescope_core::testing::{scratch_dir, FakeHost};

/// A screen shown: (slot, size, picture, mirrored).
type Shown = (u32, (u32, u32), Option<String>, bool);

/// What the fake screens were told, as (slot, what).
#[derive(Default)]
struct Log {
    shown: Vec<Shown>,
    stops: Vec<u32>,
    watched: Vec<(u32, bool)>,
}

struct FakeScreen {
    slot: u32,
    log: Arc<Mutex<Log>>,
}

impl Held for FakeScreen {
    fn start(&self) {}
    fn stop(&self) {
        self.log.lock().unwrap().stops.push(self.slot);
    }
    fn running(&self) -> bool {
        true
    }
}

impl WaitScreen for FakeScreen {
    fn show(&self, width: u32, height: u32, path: Option<&str>, mirror: bool) {
        self.log.lock().unwrap().shown.push((self.slot, (width, height), path.map(String::from), mirror));
    }
    fn set_watched(&self, watched: bool) {
        self.log.lock().unwrap().watched.push((self.slot, watched));
    }
}

#[derive(Default)]
struct FakeWatch {
    started: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
}

impl Held for FakeWatch {
    fn start(&self) {
        self.started.store(true, Ordering::SeqCst);
    }
    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
    }
    fn running(&self) -> bool {
        self.started.load(Ordering::SeqCst) && !self.stopped.load(Ordering::SeqCst)
    }
}

type OnChange = Box<dyn Fn(bool) + Send + Sync>;

#[derive(Default)]
struct FakeOutputs {
    log: Arc<Mutex<Log>>,
    watch: FakeWatch,
    /// What the watch thread would call when an app opens or closes the camera.
    on_change: Mutex<Option<OnChange>>,
}

impl Outputs for FakeOutputs {
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
        Some(Box::new(FakeScreen { slot, log: self.log.clone() }))
    }
    fn watch(&self, on_change: Box<dyn Fn(bool) + Send + Sync>) -> Option<Box<dyn Held>> {
        *self.on_change.lock().unwrap() = Some(on_change);
        Some(Box::new(FakeWatch { started: self.watch.started.clone(), stopped: self.watch.stopped.clone() }))
    }
}

struct Rig {
    plugin: WaitScreenPlugin,
    host: FakeHost,
    outputs: Arc<FakeOutputs>,
    folder: PathBuf,
}

impl Rig {
    /// Set up and shown, as at startup once every plugin's config is in.
    fn new() -> Rig {
        Rig::with_loader(Arc::new(|path, w, h| Ok(wait_screen::load_frames(path, w, h))))
    }

    fn with_loader(loader: Loader) -> Rig {
        let outputs = Arc::new(FakeOutputs::default());
        let folder = scratch_dir("wait-screen");
        let kept = folder.clone();
        let plugin = WaitScreenPlugin::with_loader(outputs.clone(), loader).keeping_in(move || kept.clone());
        let mut rig = Rig { plugin, host: FakeHost::new(), outputs, folder };
        rig.plugin.setup(&mut rig.host);
        rig.plugin.on_plugins_ready(&mut rig.host);
        rig
    }

    fn event(&mut self, e: Event) {
        self.plugin.on_event(&mut self.host, &e);
    }

    fn idle(&mut self, slots: &[u32]) {
        self.event(Event::IdleOutputs { slots: slots.to_vec() });
    }

    fn shown(&self, slot: u32) -> Vec<((u32, u32), Option<String>, bool)> {
        self.outputs.log.lock().unwrap().shown.iter().filter(|s| s.0 == slot).map(|s| (s.1, s.2.clone(), s.3)).collect()
    }

    fn stops(&self, slot: u32) -> usize {
        self.outputs.log.lock().unwrap().stops.iter().filter(|s| **s == slot).count()
    }

    fn saves(&self) -> usize {
        self.host.calls.iter().filter(|c| *c == "schedule_save").count()
    }

    /// The files kept next to the config.
    fn kept(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.folder).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("wait_screen")).collect();
        names.sort();
        names
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "wait-screen".into(), callback: callback.into(), args });
    }

    /// The newest value the card was given for a property.
    fn card(&self, prop: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props wait-screen ")).filter_map(|p| serde_json::from_str::<Value>(p).ok())
            .find_map(|p| p.get(prop).cloned()).unwrap_or(Value::Null)
    }

    /// An app opens or lets go of the camera, as the watch's thread says it and the host brings it to the plugin.
    fn app_reads(&mut self, reads: bool) {
        (self.outputs.on_change.lock().unwrap().as_ref().expect("a watch"))(reads);
        for (token, data) in self.host.take_posted() {
            self.event(Event::Posted { owner: NAME.into(), token, data });
        }
    }
}

fn png(path: &Path, color: [u8; 3], size: (u32, u32)) {
    image::RgbImage::from_pixel(size.0, size.1, image::Rgb(color)).save(path).unwrap();
}

// test_wait_screen.py::test_shows_the_default_screen_at_start_and_watches
#[test]
fn shows_the_default_screen_at_start_and_watches() {
    let r = Rig::new();
    assert_eq!(r.shown(0), [((1920, 1080), None, false)]);
    assert!(r.outputs.watch.started.load(Ordering::SeqCst));
}

// test_wait_screen.py::test_size_follows_the_canvas_then_the_last_stream
#[test]
fn size_follows_the_canvas_then_the_last_stream() {
    let mut r = Rig::new();
    r.event(Event::VcamOpened { width: 640, height: 480 });
    assert_eq!(r.plugin.size(&r.host), (640, 480));
    assert_eq!(r.saves(), 1);
    r.event(Event::VcamOpened { width: 640, height: 480 });
    assert_eq!(r.saves(), 1);
    r.host.canvas = (Some(1280), Some(720));
    assert_eq!(r.plugin.size(&r.host), (1280, 720));
    // A canvas that's only Auto in one direction is no canvas
    r.host.canvas = (Some(1280), None);
    assert_eq!(r.plugin.size(&r.host), (640, 480));
}

// test_wait_screen.py::test_gives_the_camera_to_the_stream_and_takes_it_back
#[test]
fn gives_the_camera_to_the_stream_and_takes_it_back() {
    let mut r = Rig::new();
    r.plugin.on_stream_starting(&mut r.host);
    assert_eq!(r.stops(0), 1);
    r.host.streaming = true;
    r.plugin.show(&mut r.host);
    assert_eq!(r.shown(0).len(), 1); // never while streaming
    r.host.streaming = false;
    r.plugin.on_stream_stop(&mut r.host);
    assert_eq!(r.shown(0).len(), 2);
}

// test_wait_screen.py::test_extra_cameras_show_it_while_nothing_streams_to_them
#[test]
fn extra_cameras_show_it_while_nothing_streams_to_them() {
    let mut r = Rig::new();
    r.idle(&[1, 2]);
    assert_eq!(r.shown(1), [((1920, 1080), None, false)]);
    assert_eq!(r.shown(2), [((1920, 1080), None, false)]);

    r.host.streaming = true; // the first camera streams; a second one is about to take camera 2
    r.idle(&[1]);
    assert_eq!((r.stops(2), r.stops(1)), (1, 0));

    r.plugin.set_mirror(&mut r.host, true); // every screen picks it up
    assert!(r.shown(1).last().unwrap().2);
    assert_eq!(r.shown(2).len(), 1);

    r.idle(&[1, 2]); // that stream stopped
    assert_eq!(r.shown(2).len(), 2);
    r.plugin.shutdown(&mut r.host);
    assert_eq!((r.stops(1), r.stops(2)), (1, 2));
}

// test_wait_screen.py::test_the_main_camera_shows_it_while_only_extra_ones_stream
#[test]
fn the_main_camera_shows_it_while_only_extra_ones_stream() {
    let mut r = Rig::new();
    r.host.streaming = true;
    let before = r.shown(0).len(); // the first look, before the stream
    r.idle(&[2, 3]); // the main camera and camera 2 stream
    assert_eq!(r.shown(0).len(), before);
    r.idle(&[0, 2, 3]); // the main camera's stream stopped; camera 2's goes on
    assert_eq!(r.shown(0).len(), before + 1);
    r.idle(&[0, 2, 3]);
    assert_eq!(r.shown(0).len(), before + 1); // already up
    r.plugin.on_stream_starting(&mut r.host); // a stream takes the main camera again
    r.idle(&[2, 3]);
    assert_eq!(r.stops(0), 1);
    r.plugin.shutdown(&mut r.host);
}

// test_wait_screen.py::test_takes_the_camera_back_while_the_stream_has_its_camera_off
#[test]
fn takes_the_camera_back_while_the_stream_has_its_camera_off() {
    let mut r = Rig::new();
    r.plugin.on_stream_starting(&mut r.host);
    r.host.streaming = true;
    r.host.camera_off = true;
    r.plugin.on_camera_off(&mut r.host);
    assert_eq!(r.shown(0).len(), 2); // the call keeps a picture while the stream is just the mic
    r.plugin.on_stream_starting(&mut r.host); // the camera coming back on
    assert_eq!(r.stops(0), 2);
}

// The plugin's half; the window's flow is in app/tests/stream_camera_off.rs under the same name
#[test]
fn a_camera_off_stream_on_an_extra_camera_gets_its_wait_screen() {
    // The flow says which cameras nothing streams to (a stream that's only its mic included); the extra one shows it then
    let mut r = Rig::new();
    r.host.streaming = true;
    r.idle(&[]);
    let before = r.shown(1).len();
    r.host.camera_off = true;
    r.idle(&[1]);
    assert_eq!(r.shown(1).len(), before + 1);
    assert_eq!(r.plugin.idle, [1]);
    r.host.camera_off = false; // the camera has it back
    r.idle(&[]);
    assert_eq!(r.stops(1), 1);
    assert!(r.plugin.idle.is_empty());
    r.plugin.shutdown(&mut r.host);
}

// test_wait_screen.py::test_a_reader_is_passed_to_the_screen_and_the_bus
#[test]
fn a_reader_is_passed_to_the_screen_and_the_bus() {
    let mut r = Rig::new();
    r.app_reads(true);
    assert_eq!(r.outputs.log.lock().unwrap().watched, [(0, true)]);
    assert!(r.host.events.contains(&Event::CameraWatched { watched: true }));
    assert!(r.plugin.diagnostics().contains(&("An app is reading the camera".into(), "yes".into())));
    r.app_reads(false);
    assert_eq!(r.outputs.log.lock().unwrap().watched, [(0, true), (0, false)]);
}

// test_wait_screen.py::test_a_chosen_image_is_kept_next_to_the_config
#[test]
fn a_chosen_image_is_kept_next_to_the_config() {
    let mut r = Rig::new();
    let tmp = scratch_dir("wait-screen-source");
    let first = tmp.join("first.PNG");
    std::fs::write(&first, b"one").unwrap();
    r.plugin.set_image(&mut r.host, first.to_str()).unwrap();
    let kept = PathBuf::from(r.plugin.image_path().unwrap());
    assert_eq!((kept.parent().unwrap(), kept.extension().unwrap()), (r.folder.as_path(), std::ffi::OsStr::new("png")));
    assert_eq!(std::fs::read(&kept).unwrap(), b"one");
    assert_eq!(r.shown(0).last().unwrap().1.as_deref(), kept.to_str());

    let second = tmp.join("second.gif");
    std::fs::write(&second, b"two").unwrap();
    r.plugin.set_image(&mut r.host, second.to_str()).unwrap();
    assert!(!kept.exists()); // the old copy goes
    let second_kept = r.plugin.image_path().unwrap().to_string();
    r.plugin.set_image(&mut r.host, Some(&second_kept)).unwrap(); // choosing the kept copy itself is fine
    assert_eq!(r.plugin.image_path(), Some(second_kept.as_str()));
    assert_eq!(std::fs::read(&second_kept).unwrap(), b"two");
    assert_eq!(r.kept(), [Path::new(&second_kept).file_name().unwrap().to_string_lossy().into_owned()]);

    r.plugin.set_image(&mut r.host, None).unwrap();
    assert_eq!(r.plugin.image_path(), None);
    assert_eq!(r.shown(0).last().unwrap().1, None);
    assert_eq!(r.saves(), 4);
}

// test_wait_screen.py::test_a_second_image_of_the_same_type_is_a_different_path
#[test]
fn a_second_image_of_the_same_type_is_a_different_path() {
    let mut r = Rig::new();
    let tmp = scratch_dir("wait-screen-source");
    for (name, data) in [("a.png", b"one"), ("b.png", b"two")] {
        std::fs::write(tmp.join(name), data).unwrap();
        r.plugin.set_image(&mut r.host, tmp.join(name).to_str()).unwrap();
    }
    let shown = r.shown(0);
    let (first, second) = (shown[shown.len() - 2].1.clone().unwrap(), shown[shown.len() - 1].1.clone().unwrap());
    assert_ne!(first, second); // so the camera reloads instead of keeping the first picture
    assert_eq!(std::fs::read(&second).unwrap(), b"two");
    assert_eq!(r.kept(), [Path::new(&second).file_name().unwrap().to_string_lossy().into_owned()]);
}

// test_wait_screen.py::test_a_copy_from_an_older_version_goes_on_the_next_pick
#[test]
fn a_copy_from_an_older_version_goes_on_the_next_pick() {
    let mut r = Rig::new();
    let legacy = r.folder.join("wait_screen.png");
    std::fs::write(&legacy, b"old").unwrap();
    let tmp = scratch_dir("wait-screen-source");
    std::fs::write(tmp.join("a.png"), b"new").unwrap();
    r.plugin.set_image(&mut r.host, tmp.join("a.png").to_str()).unwrap();
    assert!(!legacy.exists());
    assert_eq!(std::fs::read(r.plugin.image_path().unwrap()).unwrap(), b"new");
}

// test_wait_screen.py::test_a_failed_replacement_keeps_the_previous_image
#[test]
fn a_failed_replacement_keeps_the_previous_image() {
    let outputs = Arc::new(FakeOutputs::default());
    let folder = scratch_dir("wait-screen");
    let kept_in = folder.clone();
    let mut plugin = WaitScreenPlugin::new(outputs.clone()).keeping_in(move || kept_in.clone());
    let mut host = FakeHost::new();
    plugin.setup(&mut host);
    let tmp = scratch_dir("wait-screen-source");
    std::fs::write(tmp.join("a.png"), b"one").unwrap();
    plugin.set_image(&mut host, tmp.join("a.png").to_str()).unwrap();
    let previous = plugin.image_path().unwrap().to_string();
    let saves = host.calls.iter().filter(|c| *c == "schedule_save").count();
    let shows = outputs.log.lock().unwrap().shown.len();
    std::fs::write(tmp.join("b.jpg"), b"two").unwrap();

    // A disk that fills up part of the way through the copy
    let mut plugin = plugin.copying_with(|_from, to| {
        std::fs::write(to, b"tw")?;
        // ERROR_DISK_FULL on Windows, ENOSPC elsewhere
        Err(std::io::Error::from_raw_os_error(if cfg!(windows) { 112 } else { 28 }))
    });
    let why = plugin.set_image(&mut host, tmp.join("b.jpg").to_str()).unwrap_err();
    assert!(why.contains(if cfg!(windows) { "not enough space" } else { "No space" }), "{why}");
    assert!(!why.contains("os error"), "{why}");
    assert_eq!(plugin.image_path(), Some(previous.as_str()));
    assert_eq!(std::fs::read(&previous).unwrap(), b"one");
    let mut left: Vec<String> = std::fs::read_dir(&folder).unwrap().flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, [Path::new(&previous).file_name().unwrap().to_string_lossy().into_owned()]); // no half-written leftovers
    assert_eq!(host.calls.iter().filter(|c| *c == "schedule_save").count(), saves);
    assert_eq!(outputs.log.lock().unwrap().shown.len(), shows);
}

// test_wait_screen.py::test_the_card_says_when_a_pick_failed_or_wont_load
#[test]
fn the_card_says_when_a_pick_failed_or_wont_load() {
    let mut r = Rig::new();
    let tmp = scratch_dir("wait-screen-source");
    // A pick that can't be kept says why and keeps what's there (the source isn't there to copy)
    r.ui("choose-clicked", vec![]);
    assert!(r.host.calls.iter().any(|c| c.starts_with("pick_file Open Choose a wait screen")));
    let request = r.plugin.picking.expect("a dialog open");
    r.event(Event::RequestDone { request, ok: true, message: String::new(), data: json!({"path": tmp.join("gone.png").to_str(), "filter": ""}) });
    let note = r.card("picture-detail");
    let gone = if cfg!(windows) { "cannot find the file specified. Keeping" } else { "No such file" };
    assert!(note.as_str().unwrap().contains(gone) && note.as_str().unwrap().contains("Keeping"), "{note}");
    assert_eq!(r.plugin.image_path(), None);

    // One that's kept but won't load shows the default screen and says so
    let bad = tmp.join("bad.png");
    std::fs::write(&bad, b"not an image").unwrap();
    r.plugin.set_image(&mut r.host, bad.to_str()).unwrap();
    r.ui("shown", vec![]);
    assert!(r.card("picture-note").as_str().unwrap().contains("Couldn't read that image"));
    // The default screen stands in
    let preview = &r.host.images[&("wait-screen".to_string(), "preview".to_string())];
    assert!(preview.width > 0 && preview.height > 0 && preview.data.iter().any(|&b| b != 0));
    assert_eq!(r.card("default-enabled"), json!(true));

    r.ui("default-clicked", vec![]);
    assert!(!r.card("picture-note").as_str().unwrap().contains("Couldn't"));
    assert_eq!(r.card("default-enabled"), json!(false));
}

// test_wait_screen.py::test_config_round_trip_rejects_odd_sizes
#[test]
fn config_round_trip_rejects_odd_sizes() {
    let cases = [
        (json!([640, 480]), Some((640, 480))),
        (json!([8, 8]), None),
        (json!([640]), None),
        (json!([true, 480]), None),
        (json!("640x480"), None),
        (json!([640.0, 480]), None),
        (json!([640, 480, 1]), None),
        (json!([640, 9000]), None),
    ];
    for (size, kept) in cases {
        let mut r = Rig::new();
        r.plugin.set_config(&json!({"image": "/x/wait_screen.png", "last_size": size})).unwrap();
        assert_eq!(r.plugin.last_size, kept, "{size}");
        assert_eq!(r.plugin.get_config(), json!({"image": "/x/wait_screen.png", "mirror": false, "last_size": kept.map(|(w, h)| vec![w, h])}), "{size}");
    }
}

// test_wait_screen.py::test_config_rejects_a_bad_image
#[test]
fn config_rejects_a_bad_image() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"image": 5})).unwrap();
    assert_eq!(r.plugin.image_path(), None);
    r.plugin.set_config(&json!({"image": ""})).unwrap();
    assert_eq!(r.plugin.image_path(), None);
    assert_eq!(r.plugin.get_config(), json!({"image": null, "mirror": false, "last_size": null}));
}

// test_wait_screen.py::test_config_keeps_mirror_only_when_it_is_really_on
#[test]
fn config_keeps_mirror_only_when_it_is_really_on() {
    for (value, mirror) in [(json!(true), true), (json!(false), false), (json!("yes"), false), (json!(null), false)] {
        let mut r = Rig::new();
        r.plugin.set_config(&json!({"mirror": value})).unwrap();
        assert_eq!((r.plugin.mirror(), r.plugin.get_config()["mirror"].clone()), (mirror, json!(mirror)), "{value}");
    }
}

// test_wait_screen.py::test_mirror_is_off_by_default_and_reshows_when_toggled
#[test]
fn mirror_is_off_by_default_and_reshows_when_toggled() {
    let mut r = Rig::new();
    assert_eq!(r.shown(0).iter().map(|s| s.2).collect::<Vec<_>>(), [false]);
    r.plugin.set_mirror(&mut r.host, true);
    assert!(r.shown(0).last().unwrap().2);
    assert_eq!(r.saves(), 1);
    r.ui("mirror-toggled", vec![json!(false)]);
    assert!(!r.plugin.mirror() && !r.shown(0).last().unwrap().2);
}

// test_wait_screen.py::test_shutdown_lets_go_for_good
#[test]
fn shutdown_lets_go_for_good() {
    let mut r = Rig::new();
    r.plugin.shutdown(&mut r.host);
    assert_eq!(r.stops(0), 1);
    assert!(r.outputs.watch.stopped.load(Ordering::SeqCst));
    r.plugin.on_stream_stop(&mut r.host);
    assert_eq!(r.shown(0).len(), 1);
}

// test_wait_screen.py::test_settings_section_goes_in_streaming_and_shows_the_wait_screen
#[test]
fn settings_section_goes_in_streaming_and_shows_the_wait_screen() {
    let mut r = Rig::new();
    let sections = r.plugin.settings_sections();
    assert_eq!((sections[0].tab.as_str(), sections[0].order, sections[0].ui.component.as_str()), ("streaming", 10, "WaitScreenSettings"));
    assert!(sections[0].keywords.contains("frozen"));
    r.ui("shown", vec![]);
    assert_eq!(r.card("default-enabled"), json!(false));
    let picture = &r.host.images[&("wait-screen".to_string(), "preview".to_string())];
    assert_eq!((picture.width, picture.height, picture.format), (384, 216, PixelFormat::Rgba));
    assert_eq!(r.card("mirror-checked"), json!(false));
    assert_eq!(r.card("picture-note"), "Shown at 1920 \u{d7} 1080");
    assert_eq!(r.card("picture-detail"), "The size the stream opens the camera at. Still images and GIFs work; transparent parts show the dark background.");
    r.ui("mirror-toggled", vec![json!(true)]);
    assert!(r.plugin.mirror());
    // The picture follows the camera's shape
    r.event(Event::VcamOpened { width: 640, height: 480 });
    r.ui("shown", vec![]);
    assert_eq!(r.card("preview-height"), json!(288));
}

#[test]
fn an_animation_plays_in_the_card_and_stops_when_it_is_shown_again() {
    let mut r = Rig::new();
    let gif = r.folder.join("wait_screen-anim.gif");
    {
        use image::codecs::gif::GifEncoder;
        let mut encoder = GifEncoder::new(std::fs::File::create(&gif).unwrap());
        for color in [[255, 0, 0, 255], [0, 0, 255, 255]] {
            let frame = image::Frame::from_parts(image::RgbaImage::from_pixel(4, 2, image::Rgba(color)), 0, 0, image::Delay::from_numer_denom_ms(200, 1));
            encoder.encode_frame(frame).unwrap();
        }
    }
    r.plugin.set_config(&json!({"image": gif.to_str()})).unwrap();
    r.ui("shown", vec![]);
    assert!(r.host.is_scheduled(TICK), "the second picture is waited for");
    let first = r.host.images[&("wait-screen".to_string(), "preview".to_string())].data.clone();
    r.event(Event::TimerFired { owner: NAME.into(), token: TICK.into() });
    let second = r.host.images[&("wait-screen".to_string(), "preview".to_string())].data.clone();
    assert_ne!(first, second);
    r.event(Event::TimerFired { owner: NAME.into(), token: TICK.into() });
    assert_eq!(r.host.images[&("wait-screen".to_string(), "preview".to_string())].data, first, "round to the start again");
    // Shown again with a still picture, nothing keeps playing
    r.plugin.set_config(&json!({"image": null})).unwrap();
    r.ui("shown", vec![]);
    assert!(!r.host.is_scheduled(TICK));
}

#[test]
fn a_picture_chosen_in_the_dialog_is_kept_and_shown() {
    let mut r = Rig::new();
    let tmp = scratch_dir("wait-screen-source");
    png(&tmp.join("logo.png"), [10, 200, 30], (16, 9));
    r.ui("choose-clicked", vec![]);
    let request = r.plugin.picking.unwrap();
    // Another request's answer isn't this one's
    r.event(Event::RequestDone { request: RequestId(request.0 + 40), ok: true, message: String::new(), data: json!({"path": tmp.join("logo.png").to_str()}) });
    assert_eq!(r.plugin.image_path(), None);
    r.event(Event::RequestDone { request, ok: true, message: String::new(), data: json!({"path": tmp.join("logo.png").to_str()}) });
    assert!(r.plugin.image_path().is_some());
    assert_eq!(r.card("default-enabled"), json!(true));
    assert!(r.card("picture-note").as_str().unwrap().starts_with("Shown at"));
    // Cancelling the dialog leaves it alone
    r.ui("choose-clicked", vec![]);
    let request = r.plugin.picking.unwrap();
    let before = r.plugin.image_path().map(String::from);
    r.event(Event::RequestDone { request, ok: false, message: String::new(), data: json!(null) });
    assert_eq!(r.plugin.image_path().map(String::from), before);
}

// ── In place of the stream's picture ──────────────────────────────────

struct Ctx;

impl FrameContext for Ctx {
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

fn blank(w: u32, h: u32, format: PixelFormat) -> Frame {
    Frame::new(w, h, format, vec![0; format.size(w, h)])
}

/// The step the stream would run, as the host asks for it.
fn step_of(plugin: &mut WaitScreenPlugin) -> Option<Box<dyn CpuStep>> {
    match plugin.frame_step()? {
        FrameStep::Cpu(step) => Some(step),
        FrameStep::Shader(_) | FrameStep::Either(..) => panic!("a shader"),
    }
}

/// What the stream sends once the wait screen's frames for it are made (on their thread).
fn covered(plugin: &mut WaitScreenPlugin, frame: &Frame) -> Frame {
    let mut step = step_of(plugin).expect("covering");
    let mut first = frame.clone();
    step.process(&mut first, &Ctx);
    for _ in 0..500 {
        if !plugin.covers.loading() {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    let mut out = frame.clone();
    step.process(&mut out, &Ctx);
    out
}

fn answers(host: &FakeHost) -> Vec<bool> {
    host.events.iter().filter_map(|e| match e { Event::StreamCovered { covered } => Some(*covered), _ => None }).collect()
}

// test_wait_screen.py::test_it_stands_in_for_the_streams_picture_when_asked
#[test]
fn it_stands_in_for_the_streams_picture_when_asked() {
    let mut r = Rig::new();
    let frame = blank(320, 180, PixelFormat::I420);
    assert!(r.plugin.frame_step().is_none());
    r.event(Event::CoverRequested { owner: "Blur".into(), on: true, note: None });
    assert_eq!(answers(&r.host), [true]);
    assert!(r.host.calls.iter().any(|c| c == "refresh_frame_step"));
    let shown = covered(&mut r.plugin, &frame);
    assert_eq!((shown.width, shown.height, shown.format), (320, 180, PixelFormat::I420));
    assert_ne!(shown.data, frame.data, "the default screen, at the stream's size");
    r.event(Event::CoverRequested { owner: "Other".into(), on: true, note: None });
    r.event(Event::CoverRequested { owner: "Blur".into(), on: false, note: None });
    assert_eq!(answers(&r.host), [true, true, true]);
    assert!(r.plugin.frame_step().is_some(), "someone still wants it");
    r.event(Event::CoverRequested { owner: "Other".into(), on: false, note: None });
    assert_eq!(answers(&r.host).last(), Some(&false));
    assert!(r.plugin.frame_step().is_none());
}

#[test]
fn a_cover_can_carry_its_own_line_under_the_name_on_the_default_screen() {
    let frame = blank(640, 360, PixelFormat::I420);
    let usual = |r: &mut Rig| {
        r.event(Event::CoverRequested { owner: "Blur".into(), on: true, note: None });
        covered(&mut r.plugin, &frame)
    };
    let plain = usual(&mut Rig::new());
    let mut r = Rig::new();
    r.event(Event::CoverRequested { owner: "Blur".into(), on: true, note: Some("Paused for privacy".into()) });
    let noted = covered(&mut r.plugin, &frame);
    // Without a font on this machine no line is drawn at all
    if wait_screen::note_screen(640, 360, "Paused for privacy") != wait_screen::default_screen(640, 360) {
        assert_ne!(noted.data, plain.data);
    }
    // Letting go takes the line with it
    r.event(Event::CoverRequested { owner: "Blur".into(), on: false, note: None });
    r.event(Event::CoverRequested { owner: "Blur".into(), on: true, note: None });
    assert_eq!(covered(&mut r.plugin, &frame).data, plain.data);
}

// test_wait_screen.py::test_the_stand_in_is_mirrored_and_coloured_like_the_camera_shows_it
#[test]
fn the_stand_in_is_mirrored_and_coloured_like_the_camera_shows_it() {
    // Red on the left of a 4 x 2 picture
    let mut data = vec![0u8; 4 * 2 * 3];
    for y in 0..2 {
        data[y * 12..y * 12 + 3].copy_from_slice(&[255, 0, 0]);
    }
    let picture = Rgb { width: 4, height: 2, data };
    let mut r = Rig::with_loader(Arc::new(move |_, _, _| Ok(vec![(picture.clone(), 1.0)])));
    r.event(Event::CoverRequested { owner: "Blur".into(), on: true, note: None });
    let at = |f: &Frame, x: usize| f.data[x * 4..x * 4 + 4].to_vec();
    let out = covered(&mut r.plugin, &blank(4, 2, PixelFormat::Rgba));
    assert_eq!(at(&out, 0), [255, 0, 0, 255]);
    // In BGRA the red is the third byte
    let out = covered(&mut r.plugin, &blank(4, 2, PixelFormat::Bgra));
    assert_eq!(at(&out, 0), [0, 0, 255, 255]);
    r.plugin.set_mirror(&mut r.host, true);
    let out = covered(&mut r.plugin, &blank(4, 2, PixelFormat::Rgba));
    assert_eq!(at(&out, 3), [255, 0, 0, 255]);
    assert_eq!(at(&out, 0), [0, 0, 0, 255]);
}

// test_wait_screen.py::test_a_slow_wait_screen_never_holds_up_the_stream
#[test]
fn a_slow_wait_screen_never_holds_up_the_stream() {
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let gate = release.clone();
    let mut r = Rig::with_loader(Arc::new(move |_, w, h| {
        // A long GIF at 1080p
        let (open, cv) = &*gate;
        let _ = cv.wait_timeout_while(open.lock().unwrap(), Duration::from_secs(5), |o| !*o);
        Ok(vec![(Rgb { width: w, height: h, data: vec![200; (w * h * 3) as usize] }, 1.0)])
    }));
    r.event(Event::CoverRequested { owner: "Blur".into(), on: true, note: None });
    let frame = Frame::new(64, 36, PixelFormat::Rgba, vec![99; 64 * 36 * 4]);
    let mut step = step_of(&mut r.plugin).unwrap();
    let mut out = frame.clone();
    let started = Instant::now();
    step.process(&mut out, &Ctx);
    assert!(started.elapsed() < Duration::from_millis(500));
    assert!(out.data.as_chunks::<4>().0.iter().all(|p| p[..3] == [0, 0, 0]), "black until it's made, never the camera's picture");
    {
        let (open, cv) = &*release;
        *open.lock().unwrap() = true;
        cv.notify_all();
    }
    let done = covered(&mut r.plugin, &frame);
    assert!(done.data.as_chunks::<4>().0.iter().all(|p| p[..3] == [200, 200, 200]));
}

// test_wait_screen.py::test_a_wait_screen_that_wont_load_is_not_retried_on_every_frame
#[test]
fn a_wait_screen_that_wont_load_is_not_retried_on_every_frame() {
    let tries = Arc::new(Mutex::new(Vec::new()));
    let seen = tries.clone();
    let mut r = Rig::with_loader(Arc::new(move |_, w, h| {
        seen.lock().unwrap().push((w, h));
        Err("unreadable".into())
    }));
    r.event(Event::CoverRequested { owner: "Blur".into(), on: true, note: None });
    let frame = Frame::new(64, 36, PixelFormat::I420, vec![200; PixelFormat::I420.size(64, 36)]);
    for _ in 0..5 {
        // Black, never the camera's picture
        let out = covered(&mut r.plugin, &frame);
        let (luma, chroma) = out.data.split_at(64 * 36);
        assert!(luma.iter().all(|&y| y == 16) && chroma.iter().all(|&c| c == 128));
    }
    let count = |tries: &Arc<Mutex<Vec<(u32, u32)>>>| tries.lock().unwrap().iter().filter(|t| **t == (64, 36)).count();
    // The warm-up at the canvas size was one try, and the frames at 64 x 36 made one of their own
    assert_eq!(count(&tries), 1);
    // Ten seconds on, it's tried again
    {
        let mut cache = r.plugin.covers.cache.lock().unwrap();
        for (_, at) in cache.failed.iter_mut() {
            *at -= RETRY + Duration::from_secs(1);
        }
    }
    covered(&mut r.plugin, &frame);
    assert_eq!(count(&tries), 2);
}

#[test]
fn changing_the_picture_while_covering_asks_for_a_new_step() {
    let mut r = Rig::new();
    r.event(Event::CoverRequested { owner: "Blur".into(), on: true, note: None });
    let before = r.host.calls.iter().filter(|c| *c == "refresh_frame_step").count();
    r.plugin.set_mirror(&mut r.host, true);
    assert_eq!(r.host.calls.iter().filter(|c| *c == "refresh_frame_step").count(), before + 1);
    // Not covering: nothing to ask for
    r.event(Event::CoverRequested { owner: "Blur".into(), on: false, note: None });
    let before = r.host.calls.iter().filter(|c| *c == "refresh_frame_step").count();
    r.plugin.set_mirror(&mut r.host, false);
    assert_eq!(r.host.calls.iter().filter(|c| *c == "refresh_frame_step").count(), before);
}

#[test]
fn the_card_compiles_and_has_what_the_plugin_uses() {
    crate::ui_check::compiles(UI, "wait_screen.slint", "WaitScreenSettings");
    let mut plugin = WaitScreenPlugin::new(Arc::new(FakeOutputs::default()));
    let section = plugin.settings_sections().remove(0);
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    let result = {
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        let mut f = std::pin::pin!(compiler.build_from_source(UI.to_string(), PathBuf::from("/root/wait_screen.slint")));
        loop {
            if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
                break v;
            }
        }
    };
    i_slint_backend_testing::init_no_event_loop();
    let instance = result.component("WaitScreenSettings").expect("the component").create().expect("an instance");
    // What the plugin sets, to start with and as it refreshes
    let mut r = Rig::new();
    r.ui("shown", vec![]);
    let mut names: Vec<String> = section.ui.props.as_object().unwrap().keys().cloned().collect();
    for call in &r.host.calls {
        if let Some(props) = call.strip_prefix("set_ui_props wait-screen ") {
            names.extend(serde_json::from_str::<Value>(props).unwrap().as_object().unwrap().keys().cloned());
        }
    }
    for name in names {
        assert!(instance.get_property(&name).is_ok(), "the card has no property {name}");
    }
    assert!(instance.get_property("preview").is_ok(), "the picture goes in its property");
    for callback in ["choose-clicked", "default-clicked", "mirror-toggled"] {
        assert!(instance.set_callback(callback, |_| slint_interpreter::Value::Void).is_ok(), "the card has no callback {callback}");
    }
}

#[test]
fn it_is_a_cover_stage_plugin_in_on_screen_that_can_be_switched_off() {
    let info = WaitScreenPlugin::new(Arc::new(FakeOutputs::default())).info();
    assert_eq!((info.name.as_str(), info.category.as_str(), info.icon.as_str(), info.frame_stage), ("wait_screen", "on_screen", "moon", FrameStage::Cover));
    assert!(!info.required && !info.optional);
}

/// A stream in the Windows camera's format is covered too: the real picture must never show through.
#[test]
fn an_nv12_stream_is_covered_like_any_other() {
    let mut r = Rig::new();
    let frame = Frame::new(320, 180, PixelFormat::Nv12, vec![200; PixelFormat::Nv12.size(320, 180)]);
    r.event(Event::CoverRequested { owner: "Blur".into(), on: true, note: None });
    let out = covered(&mut r.plugin, &frame);
    assert_eq!(out.format, PixelFormat::Nv12);
    assert_ne!(out.data, frame.data);
}
