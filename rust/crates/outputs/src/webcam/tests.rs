use super::*;
use crate::webcams::Mode;
use telescope_core::testing::FakeHost;

fn modes() -> Vec<Mode> {
    vec![Mode::new("YUYV", 640, 480, &[30.0]), Mode::new("YUYV", 1920, 1080, &[5.0]),
         Mode::new("MJPG", 1280, 720, &[30.0, 60.0]), Mode::new("MJPG", 1920, 1080, &[30.0])]
}

fn c920() -> Device {
    Device { key: "usb-c920".into(), name: "HD Pro Webcam C920".into(), location: "/dev/video0".into(), modes: modes() }
}

fn brio() -> Device {
    Device { key: "usb-brio".into(), name: "Brio".into(), location: "/dev/video2".into(), modes: modes() }
}

struct Env {
    plugin: WebcamPlugin,
    host: FakeHost,
    plugged: Arc<Mutex<Vec<Device>>>,
}

fn cams_with(plugged: Vec<Device>, controls: ControlsFor) -> Env {
    *crate::webcam_controls::tests::TERRITORY.lock().unwrap() = Some("GB".into());
    let plugged = Arc::new(Mutex::new(plugged));
    let p = plugged.clone();
    let system = System {
        opener: Arc::new(|_, _| Err((false, "no camera in tests".into()))),
        users: Arc::new(|_| String::new()),
        exists: Arc::new(|_| true),
    };
    let mut plugin = WebcamPlugin::new(Box::new(move || Ok(p.lock().unwrap().clone())), controls, system);
    let mut host = FakeHost::new();
    plugin.setup(&mut host);
    plugin.on_event(&mut host, &Event::TimerFired { owner: NAME.into(), token: T_SCAN.into() });
    Env { plugin, host, plugged }
}

fn cams() -> Env {
    cams_with(vec![c920()], Box::new(|_| None))
}

fn pick(env: &mut Env, key: Option<&str>) {
    let sid = key.map(|k| format!("{SOURCE_PREFIX}{k}")).unwrap_or_default();
    env.plugin.on_event(&mut env.host, &Event::SourceSelected { source_id: sid });
}

fn stream(env: &mut Env, key: &str) {
    env.host.streaming = true;
    env.host.focused = Some(format!("{SOURCE_PREFIX}{key}"));
}

fn report(env: &mut Env, key: &str, state: &str, detail: &str) {
    env.plugin.on_event(&mut env.host, &Event::Posted { owner: NAME.into(), token: P_REPORTED.into(), data: json!([key, state, detail]) });
}

fn status(env: &Env) -> String {
    let streaming = env.host.streaming;
    env.plugin.props(streaming)["status-text"].as_str().unwrap().to_string()
}

// test_webcam_plugin.py::test_webcams_are_listed_by_name
#[test]
fn webcams_are_listed_by_name() {
    let env = cams();
    assert!(env.host.calls.contains(&"add_stream_source webcam:usb-c920".to_string()));
    assert_eq!(env.plugin.sources["usb-c920"].name(), "HD Pro Webcam C920");
}

// test_webcam_plugin.py::test_a_webcam_plugged_in_joins_the_picker_and_one_pulled_out_leaves
#[test]
fn a_webcam_plugged_in_joins_the_picker_and_one_pulled_out_leaves() {
    let mut env = cams();
    env.plugged.lock().unwrap().push(brio());
    env.plugin.rescan(&mut env.host);
    assert!(env.plugin.sources.contains_key("usb-brio"));
    env.plugged.lock().unwrap().retain(|d| d.key != "usb-brio");
    env.plugin.rescan(&mut env.host);
    assert!(!env.plugin.sources.contains_key("usb-brio"));
    assert!(env.host.calls.contains(&"remove_stream_source webcam:usb-brio".to_string()), "{:?}", env.host.calls);
}

// test_webcam_plugin.py::test_a_renamed_webcam_is_offered_again_under_its_new_name
#[test]
fn a_renamed_webcam_is_offered_again_under_its_new_name() {
    let mut env = cams();
    env.plugged.lock().unwrap().push(Device { key: "usb-c920-2".into(), name: "HD Pro Webcam C920 (2)".into(), location: "/dev/video4".into(), modes: modes() });
    env.plugin.rescan(&mut env.host);
    assert_eq!(env.plugin.sources["usb-c920-2"].name(), "HD Pro Webcam C920 (2)");
}

#[test]
fn a_webcam_whose_name_changes_under_the_same_key_is_named_afresh() {
    let mut env = cams();
    env.plugged.lock().unwrap()[0].name = "HD Pro Webcam C920 (2)".into();
    env.plugin.rescan(&mut env.host);
    assert_eq!(env.plugin.sources["usb-c920"].name(), "HD Pro Webcam C920 (2)");
    assert_eq!(env.host.calls.iter().filter(|c| *c == "add_stream_source webcam:usb-c920").count(), 2);
}

// test_webcam_plugin.py::test_a_streaming_webcam_that_is_unplugged_waits_for_it_and_carries_on
#[test]
fn a_streaming_webcam_that_is_unplugged_waits_for_it_and_carries_on() {
    let mut env = cams();
    stream(&mut env, "usb-c920");
    pick(&mut env, Some("usb-c920"));
    env.plugged.lock().unwrap().clear();
    env.plugin.rescan(&mut env.host);
    assert!(env.plugin.sources.contains_key("usb-c920"));
    assert_eq!(status(&env), "Not plugged in");
    let src = env.plugin.sources["usb-c920"].clone();
    assert!(src.open_feed().is_err());
    env.plugged.lock().unwrap().push(c920());
    env.plugin.rescan(&mut env.host);
    assert!(env.plugin.shared.device("usb-c920").is_some());
    assert!(!env.plugin.states.contains_key("usb-c920"));
}

// test_webcam_plugin.py::test_an_unplugged_webcam_leaves_once_its_stream_stops
#[test]
fn an_unplugged_webcam_leaves_once_its_stream_stops() {
    let mut env = cams();
    stream(&mut env, "usb-c920");
    env.plugged.lock().unwrap().clear();
    env.plugin.rescan(&mut env.host);
    env.host.streaming = false;
    env.plugin.on_event(&mut env.host, &Event::StreamsChanged { count: 0 });
    assert!(!env.plugin.sources.contains_key("usb-c920"));
}

// test_webcam_plugin.py::test_start_on_a_webcam_unplugged_since_it_was_picked_says_so
#[test]
fn start_on_a_webcam_unplugged_since_it_was_picked_says_so() {
    let env = cams();
    let src = env.plugin.sources["usb-c920"].clone();
    assert!(src.prepare(true));
    env.plugin.shared.devices.lock().unwrap().clear();
    assert!(!src.prepare(true));
    let posted = env.host.take_posted();
    assert!(posted.iter().any(|(t, _)| t == P_UNPLUGGED), "{posted:?}");
    let mut env = env;
    env.plugin.on_event(&mut env.host, &Event::Posted { owner: NAME.into(), token: P_UNPLUGGED.into(), data: json!("HD Pro Webcam C920") });
    assert!(env.host.calls.iter().any(|c| c == "show_issue start HD Pro Webcam C920 isn't plugged in"));
}

// test_webcam_plugin.py::test_a_busy_webcam_gets_a_banner_naming_the_app_until_it_opens
#[test]
fn a_busy_webcam_gets_a_banner_naming_the_app_until_it_opens() {
    let mut env = cams();
    stream(&mut env, "usb-c920");
    pick(&mut env, Some("usb-c920"));
    report(&mut env, "usb-c920", "busy", "zoom");
    let issue = &env.host.issues.iter().find(|(k, _)| k == "webcam:usb-c920").expect("a banner").1;
    assert!(issue.title.contains("HD Pro Webcam C920") && issue.text.contains("zoom"), "{issue:?}");
    assert_eq!(status(&env), "In use by zoom");
    report(&mut env, "usb-c920", "ok", "1280×720, MJPEG, 30 fps");
    assert!(!env.host.issues.iter().any(|(k, _)| k == "webcam:usb-c920"));
    assert_eq!(status(&env), "1280×720, MJPEG, 30 fps");
}

// test_webcam_plugin.py::test_a_busy_webcam_without_a_known_app_still_says_what_to_do
#[test]
fn a_busy_webcam_without_a_known_app_still_says_what_to_do() {
    let mut env = cams();
    stream(&mut env, "usb-c920");
    report(&mut env, "usb-c920", "busy", "");
    let issue = &env.host.issues.iter().find(|(k, _)| k == "webcam:usb-c920").expect("a banner").1;
    assert!(issue.text.contains("Another app"), "{issue:?}");
}

// test_webcam_plugin.py::test_the_card_shows_only_while_a_webcam_is_picked
#[test]
fn the_card_shows_only_while_a_webcam_is_picked() {
    let mut env = cams();
    assert_eq!(env.plugin.props(false)["visible"], false);
    pick(&mut env, Some("usb-c920"));
    assert_eq!(env.plugin.props(false)["visible"], true);
    pick(&mut env, None);
    assert_eq!(env.plugin.props(false)["visible"], false);
}

// test_webcam_plugin.py::test_the_card_offers_the_cameras_sizes_and_the_rates_at_each
#[test]
fn the_card_offers_the_cameras_sizes_and_the_rates_at_each() {
    let mut env = cams();
    pick(&mut env, Some("usb-c920"));
    assert_eq!(env.plugin.props(false)["size-items"], json!(["Auto", "1920×1080", "1280×720", "640×480"]));
    assert_eq!(env.plugin.fps_items().0, [60, 30, 5]);
    env.plugin.on_ui(&mut env.host, &UiEvent { ui: "card".into(), callback: "size-picked".into(), args: vec![json!(3)] });
    assert_eq!(env.plugin.fps_items().0, [30]);
}

// test_webcam_plugin.py::test_settings_are_kept_per_camera_and_used_for_the_capture
#[test]
fn settings_are_kept_per_camera_and_used_for_the_capture() {
    let mut env = cams();
    env.plugged.lock().unwrap().push(brio());
    env.plugin.rescan(&mut env.host);
    pick(&mut env, Some("usb-c920"));
    env.plugin.on_ui(&mut env.host, &UiEvent { ui: "card".into(), callback: "size-picked".into(), args: vec![json!(2)] });
    let i = env.plugin.fps_items().0.iter().position(|f| *f == 60).unwrap();
    env.plugin.on_ui(&mut env.host, &UiEvent { ui: "card".into(), callback: "fps-picked".into(), args: vec![json!(i)] });
    let want = Choice { fourcc: "MJPG".into(), width: 1280, height: 720, fps: 60.0 };
    assert_eq!(env.plugin.shared.choice(&c920()), Some(want.clone()));
    assert_eq!(env.plugin.sources["usb-c920"].fps(), 60);
    assert_eq!(env.plugin.shared.choice(&brio()), Some(Choice { fourcc: "MJPG".into(), width: 1920, height: 1080, fps: 30.0 }));
    assert!(env.host.calls.iter().filter(|c| *c == "schedule_save").count() >= 2);
    let cfg = env.plugin.get_config();
    let mut other = cams();
    other.plugin.set_config(&cfg).unwrap();
    assert_eq!(other.plugin.shared.choice(&c920()), Some(want));
}

// test_webcam_plugin.py::test_changing_a_streaming_webcams_settings_reopens_it
#[test]
fn changing_a_streaming_webcams_settings_reopens_it() {
    let mut env = cams();
    stream(&mut env, "usb-c920");
    pick(&mut env, Some("usb-c920"));
    env.plugin.on_ui(&mut env.host, &UiEvent { ui: "card".into(), callback: "size-picked".into(), args: vec![json!(3)] });
    assert!(env.host.calls.contains(&"reconnect_stream".to_string()));
}

// test_webcam_plugin.py::test_a_saved_size_the_camera_no_longer_offers_stays_shown
#[test]
fn a_saved_size_the_camera_no_longer_offers_stays_shown() {
    let mut env = cams();
    env.plugin.set_config(&json!({"cameras": {"usb-c920": {"size": [800, 600], "fps": 30}}})).unwrap();
    pick(&mut env, Some("usb-c920"));
    let p = env.plugin.props(false);
    assert_eq!(p["size-items"][p["size-current"].as_u64().unwrap() as usize], "800×600");
}

// test_webcam_plugin.py::test_bad_config_is_ignored
#[test]
fn bad_config_is_ignored() {
    let mut env = cams();
    env.plugin.set_config(&json!({"cameras": {"usb-c920": {"size": "big", "fps": -3}, "5": {}, "x": "y"}})).unwrap();
    assert_eq!(env.plugin.shared.wanted("usb-c920"), (None, 30));
    env.plugin.set_config(&json!({"cameras": []})).unwrap();
    assert_eq!(env.plugin.get_config(), json!({"cameras": {}}));
}

// test_webcam_plugin.py::test_a_camera_that_lists_no_modes_gets_what_was_picked
#[test]
fn a_camera_that_lists_no_modes_gets_what_was_picked() {
    let env = cams_with(vec![], Box::new(|_| None));
    let bare = Device { key: "bare".into(), name: "Capture".into(), location: "0".into(), modes: vec![] };
    assert_eq!(env.plugin.shared.choice(&bare), Some(Choice { fourcc: "RAW".into(), width: 1280, height: 720, fps: 30.0 }));
}

// test_webcam_plugin.py::test_the_source_reads_through_the_plugin
#[test]
fn the_source_reads_through_the_plugin() {
    let env = cams();
    let src = env.plugin.sources["usb-c920"].clone();
    let control = src.control_client().unwrap();
    assert!(!control.has_audio());
    assert!(control.state().is_err(), "no phone to ask");
    assert_eq!(control.send(&json!({"action": "resolution", "width": 1, "height": 1})), Ok(()));
    assert_eq!(src.auto_canvas(), None);
    assert!(src.lost_hint().contains("plugged in"));
    assert!(src.open_feed().is_err(), "the stand-in opener has no camera");
    assert_eq!(src.waiting_text(), webcam_capture::BUSY_TEXT);
}

struct OneFrame;

struct NoStop;

impl telescope_core::media::FeedStopper for NoStop {
    fn stop(&self) {}
}

impl webcam_capture::Capture for OneFrame {
    fn read(&mut self) -> Result<Vec<u8>, String> {
        Ok(vec![0xFF, 0xD8, 0xFF, 0xD9])
    }
    fn stopper(&self) -> Box<dyn telescope_core::media::FeedStopper> {
        Box::new(NoStop)
    }
}

// test_webcam_plugin.py::test_the_source_reads_through_the_plugin: with a camera there, the source gives the frames it read
#[test]
fn the_source_opens_a_feed_of_the_cameras_frames() {
    let plugged = Arc::new(Mutex::new(vec![c920()]));
    let p = plugged.clone();
    let system = System {
        opener: Arc::new(|_, _| Ok(Box::new(OneFrame) as Box<dyn webcam_capture::Capture>)),
        users: Arc::new(|_| String::new()),
        exists: Arc::new(|_| true),
    };
    let mut plugin = WebcamPlugin::new(Box::new(move || Ok(p.lock().unwrap().clone())), Box::new(|_| None), system);
    let mut host = FakeHost::new();
    plugin.setup(&mut host);
    plugin.on_event(&mut host, &Event::TimerFired { owner: NAME.into(), token: T_SCAN.into() });
    let src = plugin.sources["usb-c920"].clone();
    let (codec, mut feed) = src.open_feed().expect("a feed from the camera");
    assert_eq!(codec, telescope_core::media::Codec::Mjpeg);
    assert_eq!(feed.read().map(|p| p.data).unwrap(), [0xFF, 0xD8, 0xFF, 0xD9]);
}

// test_webcam_plugin.py::test_a_failing_scan_keeps_what_was_found
#[test]
fn a_failing_scan_keeps_what_was_found() {
    let mut env = cams();
    env.plugin.scan = Box::new(|| Err("broken".into()));
    env.plugin.rescan(&mut env.host);
    assert!(env.plugin.sources.contains_key("usb-c920"));
}

// test_webcam_plugin.py::test_diagnostics_list_the_webcams_and_their_streams
#[test]
fn diagnostics_list_the_webcams_and_their_streams() {
    let mut env = cams();
    report(&mut env, "usb-c920", "ok", "1280×720, MJPEG, 30 fps");
    let d: HashMap<String, String> = env.plugin.diagnostics().into_iter().collect();
    assert_eq!(d["Webcams"], "HD Pro Webcam C920 (4 modes)");
    assert!(d["Webcam streams"].contains("1280×720"));
    env.plugged.lock().unwrap().clear();
    env.plugin.rescan(&mut env.host);
    assert_eq!(env.plugin.diagnostics(), [("Webcams".to_string(), "none found".to_string())]);
}

// The picture settings card (test_webcam_controls.py's card tests)

#[derive(Clone, Default)]
struct Backend {
    values: Arc<Mutex<HashMap<String, i64>>>,
    sets: Arc<Mutex<Vec<(String, i64)>>>,
    closed: Arc<Mutex<bool>>,
}

impl Backend {
    fn new() -> Backend {
        let b = Backend::default();
        b.values.lock().unwrap().extend([("exposure_auto".to_string(), 1), ("exposure_time".into(), 250), ("gain".into(), 0),
                                          ("anti_flicker".into(), 2), ("frame_rate_drop".into(), 1)]);
        b
    }
    fn v(&self, n: &str) -> i64 {
        self.values.lock().unwrap()[n]
    }
}

impl Controls for Backend {
    fn list(&mut self) -> Vec<Control> {
        let auto = self.v("exposure_auto");
        let mut et = Control::new("exposure_time", "int", 3, 2047, 1, 250, self.v("exposure_time"));
        et.inactive = auto != 0;
        et.unit = "100us".into();
        let mut af = Control::new("anti_flicker", "menu", 0, 2, 1, 2, self.v("anti_flicker"));
        af.menu = vec![0, 1, 2];
        vec![Control::new("exposure_auto", "bool", 0, 1, 1, 1, auto), et, Control::new("gain", "int", 0, 255, 1, 0, self.v("gain")), af,
             Control::new("frame_rate_drop", "bool", 0, 1, 1, 0, self.v("frame_rate_drop"))]
    }
    fn set(&mut self, name: &str, value: i64) -> bool {
        if name == "exposure_time" && self.v("exposure_auto") != 0 {
            return false; // refused while auto exposure is on, as real cameras do
        }
        self.sets.lock().unwrap().push((name.into(), value));
        self.values.lock().unwrap().insert(name.into(), value);
        true
    }
}

impl Drop for Backend {
    fn drop(&mut self) {
        if Arc::strong_count(&self.closed) <= 2 {
            *self.closed.lock().unwrap() = true;
        }
    }
}

fn cam() -> Device {
    Device { key: "cam".into(), name: "Cam".into(), location: "/dev/video0".into(), modes: modes() }
}

fn card() -> (Env, Backend) {
    let backend = Backend::new();
    let b = backend.clone();
    let mut env = cams_with(vec![cam()], Box::new(move |_| Some(Box::new(b.clone()) as Box<dyn Controls>)));
    pick(&mut env, Some("cam"));
    (env, backend)
}

fn set(env: &mut Env, name: &str, value: i64) {
    env.plugin.set_control(&mut env.host, name, value);
}

fn row_enabled(env: &Env, name: &str) -> bool {
    env.plugin.rows().iter().find(|r| r["name"] == name).unwrap()["enabled"].as_bool().unwrap()
}

/// A DSLR's settings: what the camera allows now, held by the camera, so never saved or put back. Answers like the camera: a request it
/// takes moves the value (`taking` false: no room for it), and `working` says it hasn't finished.
#[derive(Clone)]
struct Dslr {
    offered: Arc<Mutex<Vec<Control>>>,
    sets: Arc<Mutex<Vec<(String, i64)>>>,
    taking: Arc<Mutex<bool>>,
    working: Arc<Mutex<bool>>,
    /// The choice each menu set was told it was, by name.
    labelled: Arc<Mutex<Vec<(String, String)>>>,
}

impl Default for Dslr {
    fn default() -> Dslr {
        Dslr { offered: Arc::default(), sets: Arc::default(), taking: Arc::new(Mutex::new(true)), working: Arc::default(), labelled: Arc::default() }
    }
}

impl Dslr {
    fn working(&self, on: bool) {
        *self.working.lock().unwrap() = on;
    }
    fn taking(&self, on: bool) {
        *self.taking.lock().unwrap() = on;
    }
    fn value(&self, name: &str) -> i64 {
        self.offered.lock().unwrap().iter().find(|c| c.name == name).unwrap().value
    }
    fn names(&self) -> Vec<(String, i64)> {
        self.sets.lock().unwrap().clone()
    }
}

impl Controls for Dslr {
    fn list(&mut self) -> Vec<Control> {
        self.offered.lock().unwrap().clone()
    }
    fn set(&mut self, name: &str, value: i64) -> bool {
        if !*self.taking.lock().unwrap() {
            return false;
        }
        self.sets.lock().unwrap().push((name.into(), value));
        if let Some(c) = self.offered.lock().unwrap().iter_mut().find(|c| c.name == name && c.kind == "menu") {
            c.value = value;
        }
        true
    }
    fn set_labelled(&mut self, name: &str, value: i64, label: &str) -> bool {
        self.labelled.lock().unwrap().push((name.into(), label.into()));
        self.set(name, value)
    }
    fn busy(&mut self) -> bool {
        *self.working.lock().unwrap()
    }
}

/// What a D5100 in M mode offers (probe/allconfig_M.txt), in the Nikon driver's own wording with its decimal commas.
fn dslr_controls() -> Vec<Control> {
    let mut focus = Control::new("dslr_focus", "buttons", -112, 112, 1, 1, 1);
    focus.transient = true;
    let menu = |name: &str, labels: &[&str], at: i64, unit: &str, ordered: bool| {
        let mut c = Control::new(name, "menu", 0, labels.len() as i64 - 1, 1, at, at);
        c.menu = (0..labels.len() as i64).collect();
        c.labels = labels.iter().map(|s| s.to_string()).collect();
        (c.transient, c.unit, c.ordered) = (true, unit.into(), ordered);
        c
    };
    let aperture = ["f/3,5", "f/4", "f/4,5", "f/5", "f/5,6", "f/6,3", "f/7,1", "f/8", "f/9", "f/10", "f/11", "f/13", "f/14", "f/16", "f/18", "f/20", "f/22"];
    let comp = ["-5", "-4,666", "-4,333", "-4", "-3,666", "-3,333", "-3", "-2,666", "-2,333", "-2", "-1,666", "-1,333", "-1", "-0,666", "-0,333", "0",
                "0,333", "0,666", "1", "1,333", "1,666", "2", "2,333", "2,666", "3", "3,333", "3,666", "4", "4,333", "4,666", "5"];
    let balance = ["Automatic", "Daylight", "Fluorescent", "Tungsten", "Flash", "Cloudy", "Shade", "Preset"];
    vec![focus, menu("dslr_aperture", &aperture, 4, "fstop", true), menu("dslr_exposure_comp", &comp, 15, "ev", true),
         menu("dslr_white_balance", &balance, 0, "", false)]
}

fn dslr_card() -> (Env, Dslr) {
    let backend = Dslr::default();
    *backend.offered.lock().unwrap() = dslr_controls();
    let b = backend.clone();
    let device = Device { key: "gphoto2-d5100".into(), name: "Nikon DSC D5100".into(), location: "gphoto2:usb:001,004".into(), modes: modes() };
    let mut env = cams_with(vec![device], Box::new(move |_| Some(Box::new(b.clone()) as Box<dyn Controls>)));
    pick(&mut env, Some("gphoto2-d5100"));
    (env, backend)
}

fn ui(env: &mut Env, callback: &str, args: Vec<Value>) {
    env.plugin.on_ui(&mut env.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
}

fn tile_index(env: &Env, name: &str) -> usize {
    let at = env.plugin.tiles().iter().position(|t| t["name"] == name).unwrap();
    env.plugin.tile_indexes()[at]
}

fn tick(env: &mut Env) {
    env.host.cancel_schedule(T_HOLD); // a timer that has fired is no longer waiting
    env.plugin.on_event(&mut env.host, &Event::TimerFired { owner: NAME.into(), token: T_HOLD.into() });
}

fn jog_tick(env: &mut Env) {
    env.host.cancel_schedule(T_JOG);
    env.plugin.on_event(&mut env.host, &Event::TimerFired { owner: NAME.into(), token: T_JOG.into() });
}

#[test]
fn a_dslrs_card_is_a_focus_jog_and_a_strip_of_tiles_with_no_heading_over_them() {
    let (env, _) = dslr_card();
    let rows = env.plugin.rows();
    assert_eq!(rows.len(), 1, "one kind of settings needs no heading: {rows:?}");
    let focus = &rows[0];
    assert_eq!((focus["name"].as_str(), focus["kind"].as_str(), focus["options"].as_array().unwrap().len()), (Some("dslr_focus"), Some("jog"), 3));
    let tiles = env.plugin.tiles();
    let of = |i: usize| (tiles[i]["name"].as_str().unwrap(), tiles[i]["label"].as_str().unwrap(), tiles[i]["kind"].as_str().unwrap());
    assert_eq!((of(0), of(1), of(2)), (("dslr_aperture", "Aperture", "stepper"), ("dslr_exposure_comp", "Exposure", "stepper"), ("dslr_white_balance", "White balance", "menu")));
    // The camera's own wording, tidied: f-numbers with a point and exposure in thirds with one decimal and a sign
    let options = |i: usize| tiles[i]["options"].as_array().unwrap().iter().map(|o| o.as_str().unwrap().to_string()).collect::<Vec<_>>();
    assert_eq!(options(0)[..3], ["f/3.5", "f/4", "f/4.5"]);
    assert_eq!((options(1)[0].as_str(), options(1)[1].as_str(), options(1)[15].as_str(), options(1)[16].as_str(), options(1)[30].as_str()), ("-5.0", "-4.7", "0", "+0.3", "+5.0"));
    assert_eq!(options(2)[..2], ["Automatic", "Daylight"]);
    assert_eq!(env.plugin.tile_indexes(), [4, 15, 0]);
    // Nothing for Reset to put back on a camera that holds its own settings
    assert_eq!(env.plugin.props(false)["has-controls"], false);
}

/// A webcam offering brightness (Light) and saturation (Image), as most do
struct Two;

impl Controls for Two {
    fn list(&mut self) -> Vec<Control> {
        vec![Control::new("brightness", "int", 0, 255, 1, 128, 128), Control::new("saturation", "int", 0, 255, 1, 128, 128)]
    }
    fn set(&mut self, _: &str, _: i64) -> bool {
        true
    }
}

#[test]
fn headings_name_the_kinds_of_settings_only_when_there_are_several() {
    let headings = |env: &Env| env.plugin.rows().iter().filter(|r| r["kind"] == "heading").map(|r| r["name"].as_str().unwrap().to_string()).collect::<Vec<_>>();
    let mut env = cams_with(vec![cam()], Box::new(|_| Some(Box::new(Two) as Box<dyn Controls>)));
    pick(&mut env, Some("cam"));
    assert_eq!(headings(&env), ["light", "image"]);
    let (env, _) = card();
    assert!(headings(&env).is_empty(), "Light alone says nothing a heading would add");
    assert!(env.plugin.rows().iter().any(|r| r["name"] == "gain"));
}

#[test]
fn a_dslrs_settings_are_sent_to_the_camera_but_never_saved_or_put_back() {
    let (mut env, backend) = dslr_card();
    set(&mut env, "dslr_aperture", 2);
    set(&mut env, "dslr_focus", 0);
    assert_eq!(backend.names(), [("dslr_aperture".to_string(), 2), ("dslr_focus".to_string(), 0)]);
    assert!(env.plugin.get_config()["cameras"].as_object().unwrap().is_empty());
    backend.sets.lock().unwrap().clear();
    stream(&mut env, "gphoto2-d5100");
    report(&mut env, "gphoto2-d5100", "ok", "640×426, MJPEG, 30 fps");
    env.plugin.reset_controls(&mut env.host);
    assert!(backend.sets.lock().unwrap().is_empty());
}

#[test]
fn a_dslrs_card_follows_the_mode_dial() {
    let (mut env, backend) = dslr_card();
    assert_eq!((env.plugin.rows().len(), env.plugin.tiles().len()), (1, 3));
    // Turned to a scene mode: the camera refuses everything, so the next look finds nothing to offer
    backend.offered.lock().unwrap().clear();
    env.plugin.rescan(&mut env.host);
    assert!(env.plugin.rows().is_empty() && env.plugin.tiles().is_empty());
    backend.offered.lock().unwrap().extend(dslr_controls());
    env.plugin.rescan(&mut env.host);
    assert_eq!((env.plugin.rows().len(), env.plugin.tiles().len()), (1, 3));
}

#[test]
fn an_arrow_steps_one_place_along_the_list_and_stops_at_its_ends() {
    let (mut env, backend) = dslr_card();
    ui(&mut env, "control-step", vec![json!("dslr_aperture"), json!(1)]);
    ui(&mut env, "control-step", vec![json!("dslr_aperture"), json!(1)]);
    assert_eq!((backend.value("dslr_aperture"), tile_index(&env, "dslr_aperture")), (6, 6));
    ui(&mut env, "control-step", vec![json!("dslr_aperture"), json!(-1)]);
    assert_eq!(backend.value("dslr_aperture"), 5);
    // The widest and the narrowest end: no request goes to the camera past them
    backend.sets.lock().unwrap().clear();
    for _ in 0..30 {
        ui(&mut env, "control-step", vec![json!("dslr_aperture"), json!(-1)]);
    }
    assert_eq!(backend.value("dslr_aperture"), 0);
    assert_eq!(backend.names().len(), 5, "five steps to the end and none after: {:?}", backend.names());
    // Never saved: the camera holds it
    assert!(env.plugin.get_config()["cameras"].as_object().unwrap().is_empty());
}

#[test]
fn a_step_the_camera_had_no_room_for_leaves_the_card_where_the_camera_is() {
    let (mut env, backend) = dslr_card();
    backend.taking(false);
    ui(&mut env, "control-step", vec![json!("dslr_exposure_comp"), json!(1)]);
    assert_eq!(tile_index(&env, "dslr_exposure_comp"), 15);
    backend.taking(true);
    ui(&mut env, "control-step", vec![json!("dslr_exposure_comp"), json!(1)]);
    assert_eq!(tile_index(&env, "dslr_exposure_comp"), 16);
}

#[test]
fn picking_from_the_list_goes_straight_to_that_entry() {
    let (mut env, backend) = dslr_card();
    ui(&mut env, "control-set", vec![json!("dslr_exposure_comp"), json!(18.0)]);
    assert_eq!((backend.value("dslr_exposure_comp"), tile_index(&env, "dslr_exposure_comp")), (18, 18));
    ui(&mut env, "control-set", vec![json!("dslr_white_balance"), json!(3.0)]);
    assert_eq!(backend.value("dslr_white_balance"), 3);
}

#[test]
fn a_held_arrow_steps_at_once_then_repeats_but_only_when_the_camera_is_ready() {
    let (mut env, backend) = dslr_card();
    ui(&mut env, "hold-down", vec![json!("dslr_aperture"), json!(1)]);
    assert_eq!(backend.names(), [("dslr_aperture".to_string(), 5)], "one step as it goes down");
    assert!(env.host.is_scheduled(T_HOLD));
    // Nothing more through the first pause, so a click is one step
    for _ in 0..8 {
        tick(&mut env);
    }
    assert_eq!(backend.names().len(), 1);
    // The first repeat, and the camera still busy for the ones after
    backend.working(true);
    for _ in 0..30 {
        tick(&mut env);
    }
    assert_eq!(backend.names().len(), 1, "no backlog on a camera that is still working: {:?}", backend.names());
    backend.working(false);
    tick(&mut env);
    assert_eq!(backend.names().len(), 2);
    for _ in 0..4 {
        tick(&mut env);
    }
    assert_eq!(backend.value("dslr_aperture"), 5 + backend.names().len() as i64 - 1);
    ui(&mut env, "hold-up", vec![]);
    let before = backend.names().len();
    for _ in 0..20 {
        tick(&mut env);
    }
    assert_eq!(backend.names().len(), before, "let go, so nothing more");
    assert!(!env.host.is_scheduled(T_HOLD));
}

#[test]
fn a_held_arrow_stops_at_the_end_of_its_list() {
    let (mut env, backend) = dslr_card();
    ui(&mut env, "hold-down", vec![json!("dslr_aperture"), json!(1)]);
    for _ in 0..300 {
        tick(&mut env);
    }
    assert_eq!(backend.value("dslr_aperture"), 16, "f/22 is the last");
    assert!(!env.host.is_scheduled(T_HOLD), "nothing left to wait for");
}

#[test]
fn held_long_enough_exposure_jumps_a_whole_stop_at_a_time() {
    let (mut env, backend) = dslr_card();
    ui(&mut env, "hold-down", vec![json!("dslr_exposure_comp"), json!(1)]);
    for _ in 0..27 {
        tick(&mut env);
    }
    let thirds = backend.value("dslr_exposure_comp");
    assert!(thirds > 15 && thirds <= 15 + 12, "one third a step so far: {thirds}");
    let at = backend.names().len();
    tick(&mut env);
    tick(&mut env);
    tick(&mut env);
    let jumped: Vec<i64> = backend.names()[at..].iter().map(|(_, v)| *v).collect();
    assert_eq!(jumped.first().map(|v| v - thirds), Some(3), "a stop is three thirds: {jumped:?}");
}

/// What the jog sends for a handle `offset` of the way out.
fn jogged(offset: f64) -> i64 {
    crate::webcam_controls::jog_value(crate::webcam_hold::jog_level(offset)).unwrap()
}

#[test]
fn the_jog_steps_at_once_and_keeps_going_at_the_size_of_how_far_it_is_out_but_only_when_the_camera_is_ready() {
    let (mut env, backend) = dslr_card();
    ui(&mut env, "jog", vec![json!("dslr_focus"), json!(0.3)]);
    assert_eq!(backend.names(), [("dslr_focus".to_string(), jogged(0.3))], "one step as the handle leaves the middle");
    assert!(env.host.is_scheduled(T_JOG));
    // The camera answers at once, so a step every other tick (0.1 s) at the size it was
    for _ in 0..4 {
        jog_tick(&mut env);
    }
    let sent: Vec<i64> = backend.names().iter().map(|(_, v)| *v).collect();
    assert_eq!(sent, vec![jogged(0.3); 3]);
    // Pulled further out, the next step is bigger; the handle doesn't have to move for the steps to keep coming
    ui(&mut env, "jog", vec![json!("dslr_focus"), json!(0.9)]);
    for _ in 0..4 {
        jog_tick(&mut env);
    }
    let sent: Vec<i64> = backend.names().iter().map(|(_, v)| *v).collect();
    assert!(sent[3..].iter().all(|v| *v == jogged(0.9)) && sent.len() == 5, "{sent:?}");
    assert!(jogged(0.9) > jogged(0.3), "further out is a bigger step");
    // And the other way is a step toward Near
    ui(&mut env, "jog", vec![json!("dslr_focus"), json!(-0.9)]);
    jog_tick(&mut env);
    jog_tick(&mut env);
    assert_eq!(backend.names().last().map(|(_, v)| *v), Some(-jogged(0.9)));
}

#[test]
fn a_jog_asks_for_nothing_inside_the_dead_zone_and_springing_back_stops_it() {
    let (mut env, backend) = dslr_card();
    ui(&mut env, "jog", vec![json!("dslr_focus"), json!(0.04)]);
    assert!(backend.names().is_empty() && !env.host.is_scheduled(T_JOG), "a resting hand's wobble");
    ui(&mut env, "jog", vec![json!("dslr_focus"), json!(1.0)]);
    jog_tick(&mut env);
    jog_tick(&mut env);
    assert!(backend.names().len() >= 2);
    // The handle sprang back to the middle: nothing more, and nothing left waiting
    ui(&mut env, "jog", vec![json!("dslr_focus"), json!(0.0)]);
    let before = backend.names().len();
    for _ in 0..40 {
        jog_tick(&mut env);
    }
    assert_eq!(backend.names().len(), before, "let go, so nothing more");
    assert!(!env.host.is_scheduled(T_JOG));
}

#[test]
fn a_jog_held_out_on_a_slow_camera_gets_one_step_at_a_time_and_no_backlog() {
    let (mut env, backend) = dslr_card();
    ui(&mut env, "jog", vec![json!("dslr_focus"), json!(1.0)]);
    // The first step goes at once, the camera then stays busy through a hundred ticks
    backend.working(true);
    for _ in 0..100 {
        jog_tick(&mut env);
    }
    assert_eq!(backend.names().len(), 1, "no steps stacked up behind a camera that is working: {:?}", backend.names());
    backend.working(false);
    jog_tick(&mut env);
    assert_eq!(backend.names().len(), 2, "one the moment it is ready");
    backend.working(true);
    for _ in 0..20 {
        jog_tick(&mut env);
    }
    assert_eq!(backend.names().len(), 2);
}

#[test]
fn a_click_beside_the_jog_is_one_small_step_that_way_and_the_middle_button_is_autofocus() {
    let (mut env, backend) = dslr_card();
    ui(&mut env, "control-step", vec![json!("dslr_focus"), json!(-1)]);
    ui(&mut env, "control-step", vec![json!("dslr_focus"), json!(1)]);
    assert_eq!(backend.names(), [("dslr_focus".to_string(), 0), ("dslr_focus".to_string(), 2)], "Near and Far, the small step each");
    assert!(!env.host.is_scheduled(T_JOG));
    // Focus runs autofocus once, not stepping
    backend.sets.lock().unwrap().clear();
    ui(&mut env, "control-set", vec![json!("dslr_focus"), json!(1.0)]);
    assert_eq!(backend.names(), [("dslr_focus".to_string(), 1)]);
}

#[test]
fn a_hold_ends_when_another_camera_is_picked_or_the_plugin_stops() {
    let (mut env, backend) = dslr_card();
    ui(&mut env, "hold-down", vec![json!("dslr_aperture"), json!(1)]);
    assert!(env.host.is_scheduled(T_HOLD));
    pick(&mut env, None);
    assert!(!env.host.is_scheduled(T_HOLD));
    for _ in 0..20 {
        tick(&mut env);
    }
    assert_eq!(backend.names().len(), 1);
    ui(&mut env, "hold-down", vec![json!("nothing"), json!(1)]);
    assert!(!env.host.is_scheduled(T_HOLD), "nothing to hold");
    // A jog held out ends the same ways
    let (mut env, backend) = dslr_card();
    ui(&mut env, "jog", vec![json!("dslr_focus"), json!(0.5)]);
    assert!(env.host.is_scheduled(T_JOG));
    pick(&mut env, None);
    assert!(!env.host.is_scheduled(T_JOG));
    for _ in 0..20 {
        jog_tick(&mut env);
    }
    assert_eq!(backend.names().len(), 1);
    pick(&mut env, Some("gphoto2-d5100"));
    ui(&mut env, "jog", vec![json!("dslr_focus"), json!(0.5)]);
    env.plugin.shutdown(&mut env.host);
    assert!(!env.host.is_scheduled(T_JOG));
}

#[test]
fn stepping_and_holding_send_no_new_tiles_that_would_rebuild_the_arrow_under_the_hand() {
    fn last_props(env: &Env) -> Value {
        let call = env.host.calls.iter().rev().find(|c| c.starts_with("set_ui_props card ")).expect("a card update");
        serde_json::from_str(call.strip_prefix("set_ui_props card ").unwrap()).unwrap()
    }
    let (mut env, _backend) = dslr_card();
    env.plugin.card();
    env.plugin.rescan(&mut env.host);
    assert!(last_props(&env).get("tiles").is_none() && last_props(&env).get("rows").is_none(), "nothing changed, so nothing replaced");
    ui(&mut env, "hold-down", vec![json!("dslr_aperture"), json!(1)]);
    assert!(last_props(&env).get("tiles").is_none());
    assert_eq!(last_props(&env)["tile-indexes"], json!([5, 15, 0]));
    for _ in 0..12 {
        tick(&mut env);
    }
    env.plugin.rescan(&mut env.host);
    assert!(last_props(&env).get("tiles").is_none(), "the scan every two seconds leaves them be too");
    // A scan while the camera is still working through a request doesn't read where it was before
    ui(&mut env, "hold-up", vec![]);
}

// test_webcam_controls.py::test_a_webcam_is_only_written_to_once_its_stream_opens_and_gets_anti_flicker_for_the_local_mains
#[test]
fn a_webcam_is_only_written_to_once_its_stream_opens_and_gets_anti_flicker_for_the_local_mains() {
    let (mut env, backend) = card();
    assert!(backend.sets.lock().unwrap().is_empty());
    stream(&mut env, "cam");
    report(&mut env, "cam", "ok", "1280×720, MJPEG, 30 fps");
    assert_eq!(*backend.sets.lock().unwrap(), [("anti_flicker".to_string(), 1)]);
}

// test_webcam_controls.py::test_the_card_shows_only_what_the_camera_offers_and_greys_manual_ones_under_auto
#[test]
fn the_card_shows_only_what_the_camera_offers_and_greys_manual_ones_under_auto() {
    let (mut env, _backend) = card();
    let names: Vec<String> = env.plugin.rows().iter().filter(|r| r["kind"] != "heading").map(|r| r["name"].as_str().unwrap().to_string()).collect();
    assert_eq!(names, ["exposure_auto", "exposure_time", "gain", "anti_flicker", "frame_rate_drop"]);
    assert!(!row_enabled(&env, "exposure_time"));
    set(&mut env, "exposure_auto", 0);
    assert!(row_enabled(&env, "exposure_time"));
}

// test_webcam_controls.py::test_picked_settings_are_kept_per_camera_and_put_back_when_it_opens_again
#[test]
fn picked_settings_are_kept_per_camera_and_put_back_when_it_opens_again() {
    let (mut env, backend) = card();
    set(&mut env, "exposure_auto", 0);
    set(&mut env, "exposure_time", 166);
    set(&mut env, "gain", 40);
    assert_eq!(env.plugin.get_config()["cameras"]["cam"]["controls"], json!({"exposure_auto": 0, "exposure_time": 166, "gain": 40}));
    backend.values.lock().unwrap().extend([("exposure_auto".to_string(), 1), ("exposure_time".into(), 250), ("gain".into(), 0)]);
    backend.sets.lock().unwrap().clear();
    stream(&mut env, "cam");
    report(&mut env, "cam", "ok", "1280×720, MJPEG, 30 fps");
    let sets = backend.sets.lock().unwrap().clone();
    assert_eq!(sets[0], ("exposure_auto".to_string(), 0));
    assert!(sets.contains(&("exposure_time".to_string(), 166)) && sets.contains(&("gain".to_string(), 40)));
}

// test_webcam_controls.py::test_an_opening_that_reads_like_the_last_one_still_puts_the_settings_back
#[test]
fn an_opening_that_reads_like_the_last_one_still_puts_the_settings_back() {
    let (mut env, backend) = card();
    set(&mut env, "gain", 40);
    stream(&mut env, "cam");
    report(&mut env, "cam", "ok", "1280×720, MJPEG, 30 fps");
    backend.values.lock().unwrap().insert("gain".into(), 0);
    backend.sets.lock().unwrap().clear();
    report(&mut env, "cam", "ok", "1280×720, MJPEG, 30 fps");
    assert!(backend.sets.lock().unwrap().contains(&("gain".to_string(), 40)));
}

// test_webcam_controls.py::test_reset_puts_the_cameras_defaults_back_and_forgets_what_was_picked
#[test]
fn reset_puts_the_cameras_defaults_back_and_forgets_what_was_picked() {
    let (mut env, backend) = card();
    set(&mut env, "exposure_auto", 0);
    set(&mut env, "gain", 40);
    env.plugin.reset_controls(&mut env.host);
    assert_eq!((backend.v("exposure_auto"), backend.v("gain")), (1, 0));
    assert_eq!(backend.v("anti_flicker"), 1, "the local mains, not the camera's 60 Hz");
    assert_eq!(backend.v("frame_rate_drop"), 0);
    assert!(env.plugin.get_config()["cameras"]["cam"].get("controls").is_none());
}

// test_webcam_controls.py::test_saved_settings_that_arent_numbers_are_ignored
#[test]
fn saved_settings_that_arent_numbers_are_ignored() {
    let (mut env, _backend) = card();
    env.plugin.set_config(&json!({"cameras": {"cam": {"controls": {"gain": "lots", "zoom": 2, "3": 4.5}}}})).unwrap();
    assert_eq!(env.plugin.get_config()["cameras"]["cam"]["controls"], json!({"zoom": 2}));
}

// test_webcam_controls.py::test_a_camera_whose_settings_cant_be_reached_shows_none
#[test]
fn a_camera_whose_settings_cant_be_reached_shows_none() {
    let mut env = cams_with(vec![cam()], Box::new(|_| None));
    pick(&mut env, Some("cam"));
    assert!(env.plugin.rows().is_empty());
    assert_eq!(env.plugin.props(false)["has-controls"], false);
}

#[test]
fn the_card_declares_everything_it_is_given() {
    let env = cams_with(vec![cam()], Box::new(|_| None));
    let card = include_str!("../../ui/webcam.slint");
    let props = env.plugin.props(false);
    for name in props.as_object().unwrap().keys().filter(|k| *k != "visible") {
        assert!(card.contains(&format!("> {name}")), "{name} isn't a property of the card");
    }
}

// test_webcam_controls.py::test_a_replugged_webcam_gets_a_fresh_handle_on_its_settings
#[test]
fn a_replugged_webcam_gets_a_fresh_handle_on_its_settings() {
    let made: Arc<Mutex<Vec<Backend>>> = Arc::default();
    let m = made.clone();
    let mut env = cams_with(vec![cam()], Box::new(move |_| {
        let b = Backend::new();
        m.lock().unwrap().push(b.clone());
        Some(Box::new(b) as Box<dyn Controls>)
    }));
    env.plugin.read_controls("cam");
    stream(&mut env, "cam");
    env.plugged.lock().unwrap().clear();
    env.plugin.rescan(&mut env.host);
    assert!(*made.lock().unwrap()[0].closed.lock().unwrap());
    env.plugged.lock().unwrap().push(cam());
    env.plugin.rescan(&mut env.host);
    env.plugin.read_controls("cam");
    assert_eq!(made.lock().unwrap().len(), 2);
    assert!(!*made.lock().unwrap()[1].closed.lock().unwrap());
    env.plugin.shutdown(&mut env.host);
    assert!(*made.lock().unwrap()[1].closed.lock().unwrap());
}

#[test]
fn the_card_compiles_with_the_kit() {
    crate::ui_check::compiles(UI, "webcam.slint", "WebcamCard");
}

#[test]
fn exposure_moves_in_stops() {
    assert_eq!(val_to_log_pos(3.0, LOG_STEPS, 3.0, 2047.0), 0.0);
    assert_eq!(val_to_log_pos(2047.0, LOG_STEPS, 3.0, 2047.0), 1000.0);
    assert!((log_pos_to_val(500.0, LOG_STEPS, 3.0, 2047.0) - (3.0f64 * 2047.0).sqrt()).abs() < 0.01);
}

// The scan every two seconds renders the card too: a list it already shows must not be sent again, or a drag in progress is rebuilt away
#[test]
fn an_unchanged_list_of_rows_is_not_sent_again_while_a_slider_is_dragged() {
    fn last_props(env: &Env) -> Value {
        let call = env.host.calls.iter().rev().find(|c| c.starts_with("set_ui_props card ")).expect("a card update");
        serde_json::from_str(call.strip_prefix("set_ui_props card ").unwrap()).unwrap()
    }
    let (mut env, _backend) = card();
    env.plugin.card();
    env.plugin.rescan(&mut env.host);
    assert!(last_props(&env).get("rows").is_none(), "nothing changed, so no new rows");
    let at = env.plugin.rows().iter().position(|r| r["name"] == "gain").unwrap();
    let event = UiEvent { ui: "card".into(), callback: "control-set".into(), args: vec![json!("gain"), json!(30.0)] };
    env.plugin.on_ui(&mut env.host, &event);
    assert!(last_props(&env).get("rows").is_none());
    assert_eq!(last_props(&env)["readouts"][at], env.plugin.readouts()[at].as_str());
    env.plugin.rescan(&mut env.host);
    report(&mut env, "cam", "busy", "zoom");
    env.plugin.on_event(&mut env.host, &Event::StreamsChanged { count: 0 });
    assert!(last_props(&env).get("rows").is_none(), "the position the hand left is already on the card");
    // What the card doesn't know about still gets through
    env.plugin.reset_controls(&mut env.host);
    env.plugin.render(&mut env.host);
    assert!(last_props(&env).get("rows").is_some(), "a reset moved the sliders");
}

mod card_ui {
    use super::*;
    use i_slint_backend_testing::ElementHandle;
    use slint::platform::{PointerEventButton, WindowEvent};
    use slint::{LogicalPosition, ModelRc, VecModel};
    use slint_interpreter::{ComponentHandle, ComponentInstance, Struct, Value as SValue};
    use std::cell::RefCell;
    use std::rc::Rc;

    const WRAPPER: &str = r#"import { WebcamCard, ControlRow } from "plugin/webcam.slint";
export component Wrapper inherits Window {
    width: 520px;
    height: 900px;
    in property <[ControlRow]> rows;
    in property <[string]> readouts;
    in property <[ControlRow]> tiles;
    in property <[int]> tile-indexes;
    callback control-set(string, float);
    callback control-step(string, int);
    callback hold-down(string, int);
    callback hold-up();
    callback jog(string, float);
    VerticalLayout {
        alignment: start;
        WebcamCard {
            rows: root.rows; readouts: root.readouts; tiles: root.tiles; tile-indexes: root.tile-indexes;
            control-set(n, v) => { root.control-set(n, v); }
            control-step(n, d) => { root.control-step(n, d); }
            hold-down(n, d) => { root.hold-down(n, d); }
            hold-up => { root.hold-up(); }
            jog(n, v) => { root.jog(n, v); }
        }
    }
}"#;

    fn row_value(json: &Value) -> SValue {
        let mut row = json!({"name": "", "label": "", "kind": "", "icon": "", "checked": false, "options": [], "index": 0, "minimum": 0, "maximum": 0,
                             "step": 0, "position": 0, "reset-to": 0, "tip": "", "enabled": true});
        for (k, v) in json.as_object().unwrap() {
            row[k] = v.clone();
        }
        let field = |v: &Value| match v {
            Value::String(s) => SValue::String(s.as_str().into()),
            Value::Bool(b) => SValue::Bool(*b),
            Value::Number(n) => SValue::Number(n.as_f64().unwrap()),
            Value::Array(a) => SValue::Model(ModelRc::new(VecModel::from(a.iter().map(|s| SValue::String(s.as_str().unwrap().into())).collect::<Vec<_>>()))),
            other => panic!("{other}"),
        };
        SValue::Struct(Struct::from_iter(row.as_object().unwrap().iter().filter(|(k, _)| *k != "readout").map(|(k, v)| (k.clone(), field(v)))))
    }

    fn model(list: Vec<SValue>) -> SValue {
        SValue::Model(ModelRc::new(VecModel::from(list)))
    }

    /// What the card asked of the plugin, in order: (callback, control, number).
    type Asked = Rc<RefCell<Vec<(String, String, f64)>>>;

    /// The card on a plugin's props the way the shell applies them: every list is a new model.
    struct Card<B> {
        ui: ComponentInstance,
        env: Rc<RefCell<(Env, usize)>>,
        /// What the card asked since the plugin was last told
        asked: Asked,
        /// Everything it asked since the test began
        history: Asked,
        backend: B,
    }

    impl<B> Card<B> {
        fn on(env: Env, backend: B) -> Card<B> {
            i_slint_backend_testing::init_no_event_loop();
            let ui = crate::ui_check::built(UI, "webcam.slint", WRAPPER.into()).create().expect("an instance");
            let asked: Asked = Rc::default();
            for name in ["control-set", "control-step", "hold-down", "hold-up", "jog"] {
                let a = asked.clone();
                ui.set_callback(name, move |args| {
                    let text = |v: &SValue| if let SValue::String(s) = v { s.to_string() } else { String::new() };
                    let number = |v: &SValue| if let SValue::Number(n) = v { *n } else { 0.0 };
                    a.borrow_mut().push((name.to_string(), args.first().map(text).unwrap_or_default(), args.get(1).map(number).unwrap_or(0.0)));
                    SValue::Void
                })
                .unwrap();
            }
            let card = Card { ui, env: Rc::new(RefCell::new((env, 0))), asked, history: Rc::default(), backend };
            card.show();
            card
        }

        // Hands the plugin what the card asked since last time, then shows what the plugin said
        fn show(&self) {
            let asked: Vec<(String, String, f64)> = self.asked.borrow_mut().drain(..).collect();
            self.history.borrow_mut().extend(asked.iter().cloned());
            let mut guard = self.env.borrow_mut();
            let (env, seen) = &mut *guard;
            for (callback, name, v) in asked {
                let args = if callback == "hold-up" { vec![] } else { vec![json!(name), json!(v)] };
                env.plugin.on_ui(&mut env.host, &UiEvent { ui: "card".into(), callback, args });
            }
            if *seen == 0 {
                env.plugin.render(&mut env.host);
            }
            for call in &env.host.calls[*seen..] {
                let Some(props) = call.strip_prefix("set_ui_props card ") else { continue };
                let props: Value = serde_json::from_str(props).unwrap();
                if let Some(rows) = props.get("rows").and_then(Value::as_array) {
                    self.ui.set_property("rows", model(rows.iter().map(row_value).collect())).unwrap();
                }
                if let Some(tiles) = props.get("tiles").and_then(Value::as_array) {
                    self.ui.set_property("tiles", model(tiles.iter().map(row_value).collect())).unwrap();
                }
                if let Some(indexes) = props.get("tile-indexes").and_then(Value::as_array) {
                    self.ui.set_property("tile-indexes", model(indexes.iter().map(|i| SValue::Number(i.as_f64().unwrap())).collect())).unwrap();
                }
                if let Some(readouts) = props.get("readouts").and_then(Value::as_array) {
                    self.ui.set_property("readouts", model(readouts.iter().map(|s| SValue::String(s.as_str().unwrap().into())).collect())).unwrap();
                }
            }
            *seen = env.host.calls.len();
        }

        fn pointer(&self, event: WindowEvent) {
            self.ui.window().dispatch_event(event);
            self.show();
        }

        /// The middle of the first element with this label that takes room on the card.
        fn centre(&self, label: &str) -> LogicalPosition {
            let found: Vec<ElementHandle> = ElementHandle::find_by_accessible_label(&self.ui, label).filter(|e| e.size().width > 0.0).collect();
            let e = found.first().unwrap_or_else(|| panic!("no {label}"));
            let (p, s) = (e.absolute_position(), e.size());
            LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0)
        }

        fn press(&self, at: LogicalPosition) {
            self.pointer(WindowEvent::PointerMoved { position: at });
            self.pointer(WindowEvent::PointerPressed { position: at, button: PointerEventButton::Left });
        }

        fn release(&self, at: LogicalPosition) {
            self.pointer(WindowEvent::PointerReleased { position: at, button: PointerEventButton::Left });
        }

        fn asked_so_far(&self) -> Vec<(String, String, f64)> {
            self.history.borrow().clone()
        }
    }

    impl Card<Backend> {
        fn new() -> Card<Backend> {
            let (env, backend) = card();
            Card::on(env, backend)
        }
    }

    impl Card<Dslr> {
        fn dslr() -> Card<Dslr> {
            let (env, backend) = dslr_card();
            Card::on(env, backend)
        }
    }

    // A drag on a slider must keep going: the plugin answering each move must not rebuild the slider under the hand
    #[test]
    fn a_picture_slider_keeps_following_a_drag() {
        let card = Card::new();
        let slider = ElementHandle::find_by_accessible_label(&card.ui, "Gain").find(|e| e.accessible_value().is_some()).expect("the Gain slider");
        let (p, s) = (slider.absolute_position(), slider.size());
        let y = p.y + s.height / 2.0;
        let at = |x: f32| LogicalPosition::new(p.x + x, y);
        card.pointer(WindowEvent::PointerMoved { position: at(30.0) });
        card.pointer(WindowEvent::PointerPressed { position: at(30.0), button: PointerEventButton::Left });
        let mut seen = vec![card.backend.v("gain")];
        for x in [50.0, 70.0, 90.0, 110.0] {
            card.pointer(WindowEvent::PointerMoved { position: at(x) });
            seen.push(card.backend.v("gain"));
        }
        card.pointer(WindowEvent::PointerReleased { position: at(110.0), button: PointerEventButton::Left });
        assert!(seen.windows(2).all(|w| w[1] > w[0]), "each move carried the value on: {seen:?}");
    }

    // The tile for a yes/no setting is as tall as the kit makes it, whatever the hidden controls around it say
    #[test]
    fn a_switch_row_is_a_whole_tile_tall() {
        let card = Card::new();
        let tiles: Vec<_> = ElementHandle::find_by_element_type_name(&card.ui, "SwitchTile").filter(|t| t.size().height > 0.0).collect();
        assert_eq!(tiles.len(), 1, "frame_rate_drop is the one switch here");
        assert!(tiles.iter().all(|t| t.size().height == 40.0), "{:?}", tiles.iter().map(|t| t.size().height).collect::<Vec<_>>());
    }

    /// The jog on the card: its middle, and how far its handle can go either way.
    fn jog_of(card: &Card<Dslr>) -> (LogicalPosition, f32) {
        let jog = ElementHandle::find_by_element_type_name(&card.ui, "Jog").next().expect("the focus jog");
        let (p, s) = (jog.absolute_position(), jog.size());
        (LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0), s.width / 2.0 - 15.0)
    }

    // The handle follows the pointer out and tells the plugin where it is; letting go sends it back to the middle and says 0
    #[test]
    fn dragging_the_jog_out_focuses_and_letting_go_springs_it_back_and_stops() {
        let card = Card::dslr();
        let (middle, reach) = jog_of(&card);
        card.press(middle);
        assert!(card.backend.names().is_empty(), "taking hold of the handle asks for nothing");
        let at = |by: f32| LogicalPosition::new(middle.x + by, middle.y);
        card.pointer(WindowEvent::PointerMoved { position: at(reach * 0.5) });
        let half = card.backend.names();
        assert_eq!(half.len(), 1, "a step as it went out: {half:?}");
        card.pointer(WindowEvent::PointerMoved { position: at(reach + 30.0) });
        let asked: Vec<f64> = card.asked_so_far().iter().filter(|(a, _, _)| a == "jog").map(|(_, _, v)| *v).collect();
        assert!(asked.windows(2).all(|w| w[1] >= w[0]) && asked.last() == Some(&1.0), "further out, and 1 at the end however far the pointer goes: {asked:?}");
        for _ in 0..3 {
            jog_tick(&mut card.env.borrow_mut().0);
        }
        let biggest = card.backend.names().last().unwrap().1;
        assert!(biggest > half[0].1, "bigger than at half way: {} against {}", biggest, half[0].1);
        card.release(at(reach + 30.0));
        let asked = card.asked_so_far();
        assert_eq!(asked.last().map(|(a, n, v)| (a.as_str(), n.as_str(), *v)), Some(("jog", "dslr_focus", 0.0)), "let go: back to the middle");
        let before = card.backend.names().len();
        for _ in 0..20 {
            jog_tick(&mut card.env.borrow_mut().0);
        }
        assert_eq!(card.backend.names().len(), before, "and stopped");
        assert!(!card.env.borrow().0.host.is_scheduled(T_JOG));
    }

    // Dragging toward Near is Near steps, and a click on a side of the track beside the handle is a tap that way, no more
    #[test]
    fn dragging_toward_near_steps_near_and_a_click_at_a_side_is_one_small_step() {
        let card = Card::dslr();
        let (middle, reach) = jog_of(&card);
        card.press(middle);
        card.pointer(WindowEvent::PointerMoved { position: LogicalPosition::new(middle.x - reach, middle.y) });
        card.release(LogicalPosition::new(middle.x - reach, middle.y));
        assert!(card.backend.names().iter().all(|(n, v)| n == "dslr_focus" && *v < -100), "{:?}", card.backend.names());
        card.backend.sets.lock().unwrap().clear();
        let side = LogicalPosition::new(middle.x + reach * 0.6, middle.y);
        card.press(side);
        card.release(side);
        assert_eq!(card.backend.names(), [("dslr_focus".to_string(), 2)], "a click beside the track is one Far tap");
        let side = LogicalPosition::new(middle.x - reach * 0.6, middle.y);
        card.press(side);
        card.release(side);
        assert_eq!(card.backend.names().last(), Some(&("dslr_focus".to_string(), 0)), "and one Near tap");
        assert!(!card.env.borrow().0.host.is_scheduled(T_JOG));
    }

    // Focus is an ordinary button beside the jog
    #[test]
    fn the_focus_button_beside_the_jog_runs_autofocus_once() {
        let card = Card::dslr();
        let focus = ElementHandle::find_by_element_type_name(&card.ui, "Button").find(|b| b.accessible_label().as_deref() == Some("Focus") && b.size().width > 0.0).expect("the Focus button");
        let (p, s) = (focus.absolute_position(), focus.size());
        let at = LogicalPosition::new(p.x + s.width / 2.0, p.y + s.height / 2.0);
        card.press(at);
        card.release(at);
        assert_eq!(card.backend.names(), [("dslr_focus".to_string(), 1)]);
    }

    // The arrow under the hand must still be there, and still hear the release, after the plugin has answered each step
    #[test]
    fn a_held_arrow_survives_the_plugin_answering_each_step() {
        let card = Card::dslr();
        let found: Vec<ElementHandle> = ElementHandle::find_by_accessible_label(&card.ui, "Next").filter(|e| e.size().width > 0.0).collect();
        assert_eq!(found.len(), 2, "an arrow each way on the aperture and on the exposure");
        assert_eq!(found[0].accessible_description().as_deref(), Some("Aperture"), "the arrow says whose it is");
        let next = card.centre("Next");
        card.press(next);
        assert_eq!(card.backend.value("dslr_aperture"), 5, "a step as it went down");
        // The plugin's answers while it's held: the timer's steps, each followed by new indexes for the card
        for _ in 0..12 {
            tick(&mut card.env.borrow_mut().0);
            card.show();
        }
        assert!(card.backend.value("dslr_aperture") > 5);
        card.release(next);
        let asked = card.asked_so_far();
        assert_eq!(asked.last().map(|(a, _, _)| a.as_str()), Some("hold-up"), "the release got through: {asked:?}");
        assert!(!card.env.borrow().0.host.is_scheduled(T_HOLD));
    }

    // The wheel over a dial steps it one place a notch, the other way for the other turn, and a smooth wheel's small turns add up
    #[test]
    fn the_mouse_wheel_over_a_dial_steps_it() {
        let card = Card::dslr();
        let at = card.centre("Aperture");
        let scroll = |dy: f32| card.pointer(WindowEvent::PointerScrolled { position: at, delta_x: 0.0, delta_y: dy });
        scroll(60.0);
        scroll(-60.0);
        scroll(-60.0);
        assert_eq!(card.backend.value("dslr_aperture"), 3, "up one, down two from f/5.6");
        for _ in 0..5 {
            scroll(4.0);
        }
        assert_eq!(card.backend.value("dslr_aperture"), 3, "twenty pixels of smooth scrolling isn't a step yet");
        for _ in 0..5 {
            scroll(4.0);
        }
        assert_eq!(card.backend.value("dslr_aperture"), 4, "forty is");
        let asked = card.asked_so_far();
        assert!(asked.iter().all(|(a, name, _)| a == "control-step" && name == "dslr_aperture"), "{asked:?}");
    }

    // At the start of a list the arrow that would go before it can't be pressed
    #[test]
    fn the_arrow_that_would_pass_the_end_of_a_list_does_nothing() {
        let card = Card::dslr();
        let first = card.centre("Previous");
        // The aperture is at f/5.6, a few places in: step down to the widest, then press the arrow once more
        for _ in 0..4 {
            card.press(first);
            card.release(first);
        }
        assert_eq!(card.backend.value("dslr_aperture"), 0);
        let sent = card.backend.names().len();
        card.press(first);
        card.release(first);
        assert_eq!(card.backend.names().len(), sent, "f/3.5 is the first: nothing further to ask for");
    }

    #[test]
    fn the_stepper_shows_the_cameras_value_tidied_between_its_arrows() {
        let card = Card::dslr();
        let values: Vec<String> = ElementHandle::find_by_accessible_label(&card.ui, "Aperture").filter_map(|e| e.accessible_value()).map(|v| v.to_string()).collect();
        assert!(values.iter().any(|v| v == "f/5.6"), "{values:?}");
        let values: Vec<String> = ElementHandle::find_by_accessible_label(&card.ui, "Exposure").filter_map(|e| e.accessible_value()).map(|v| v.to_string()).collect();
        assert!(values.iter().any(|v| v == "0"), "{values:?}");
    }
}

fn last_card_props(env: &Env) -> Value {
    let call = env.host.calls.iter().rev().find(|c| c.starts_with("set_ui_props card ")).expect("a card update");
    serde_json::from_str(call.strip_prefix("set_ui_props card ").unwrap()).unwrap()
}

#[test]
fn a_control_that_went_with_the_mode_is_never_saved_as_a_setting_or_put_back() {
    let (mut env, backend) = dslr_card();
    // The mode dial turns and the camera stops offering the focus drive; the card hasn't looked yet
    backend.offered.lock().unwrap().retain(|c| c.name != "dslr_focus");
    ui(&mut env, "hold-down", vec![json!("dslr_focus"), json!(1)]);
    assert!(backend.names().is_empty(), "nothing is sent for a control the camera no longer has: {:?}", backend.names());
    assert!(!env.host.is_scheduled(T_HOLD));
    // Looked at again, it is gone from the card and asking for it does nothing, saves nothing
    env.plugin.rescan(&mut env.host);
    assert!(!env.plugin.rows().iter().any(|r| r["name"] == "dslr_focus"));
    assert!(!env.plugin.set_control(&mut env.host, "dslr_focus", 2));
    assert!(env.plugin.get_config()["cameras"].as_object().unwrap().is_empty(), "{}", env.plugin.get_config());
    // A jog held out when it goes ends
    let (mut env, backend) = dslr_card();
    ui(&mut env, "jog", vec![json!("dslr_focus"), json!(0.5)]);
    backend.offered.lock().unwrap().retain(|c| c.name != "dslr_focus");
    env.plugin.rescan(&mut env.host);
    jog_tick(&mut env);
    jog_tick(&mut env);
    assert!(!env.host.is_scheduled(T_JOG), "the jog ended with its control");
    assert!(env.plugin.get_config()["cameras"].as_object().unwrap().is_empty());
}

#[test]
fn a_dslr_setting_an_older_file_saved_is_not_put_back_when_it_streams() {
    let (mut env, backend) = dslr_card();
    env.plugin.set_config(&json!({"cameras": {"gphoto2-d5100": {"controls": {"dslr_focus": 0, "dslr_aperture": 3}}}})).unwrap();
    stream(&mut env, "gphoto2-d5100");
    report(&mut env, "gphoto2-d5100", "ok", "640×426, MJPEG, 30 fps");
    assert!(backend.names().is_empty(), "the camera holds its own: {:?}", backend.names());
}

#[test]
fn a_nudge_that_finds_the_list_changed_sends_the_new_tiles_and_ends_the_hold() {
    let (mut env, backend) = dslr_card();
    env.plugin.card();
    env.plugin.rescan(&mut env.host);
    ui(&mut env, "hold-down", vec![json!("dslr_exposure_comp"), json!(1)]);
    assert!(env.host.is_scheduled(T_HOLD));
    assert!(last_card_props(&env).get("tiles").is_none(), "a plain step sends no tiles");
    // The lens is zoomed: aperture now has fewer choices, and the next tick reads that
    for c in backend.offered.lock().unwrap().iter_mut().filter(|c| c.name == "dslr_aperture") {
        c.labels.drain(..2);
        c.menu = (0..c.labels.len() as i64).collect();
        (c.maximum, c.value) = (c.labels.len() as i64 - 1, 2);
    }
    for _ in 0..40 {
        tick(&mut env);
    }
    let props = last_card_props(&env);
    let tiles = props["tiles"].as_array().expect("the new list of aperture choices went out with the step");
    assert_eq!(tiles[0]["options"][0], "f/4.5", "f/3.5 and f/4 are gone");
    assert!(!env.host.is_scheduled(T_HOLD), "the arrow under the hand was rebuilt, so the hold ended");
    // And the card keeps what it was sent: a later look finds nothing to send again
    env.plugin.rescan(&mut env.host);
    assert!(last_card_props(&env).get("tiles").is_none());
}

#[test]
fn a_control_removed_during_a_dial_step_leaves_the_card_without_its_tile() {
    let (mut env, backend) = dslr_card();
    env.plugin.card();
    ui(&mut env, "control-step", vec![json!("dslr_exposure_comp"), json!(1)]);
    backend.offered.lock().unwrap().retain(|c| c.name != "dslr_aperture");
    ui(&mut env, "control-step", vec![json!("dslr_exposure_comp"), json!(1)]);
    let tiles = last_card_props(&env)["tiles"].clone();
    assert!(tiles.as_array().unwrap().iter().all(|t| t["name"] != "dslr_aperture"), "{tiles}");
    env.plugin.rescan(&mut env.host);
    assert!(last_card_props(&env).get("tiles").is_none(), "nothing left to catch up on");
}

#[test]
fn a_choice_is_sent_by_the_name_the_card_showed_not_by_its_place_in_a_list_that_may_have_changed() {
    let (mut env, backend) = dslr_card();
    ui(&mut env, "control-set", vec![json!("dslr_aperture"), json!(3.0)]);
    assert_eq!(*backend.labelled.lock().unwrap(), [("dslr_aperture".to_string(), "f/5".to_string())]);
    // A stepped one too
    ui(&mut env, "control-step", vec![json!("dslr_aperture"), json!(1)]);
    assert_eq!(backend.labelled.lock().unwrap().last().map(|(_, l)| l.as_str()), Some("f/5,6"), "{:?}", backend.labelled.lock().unwrap());
}

#[test]
fn a_choice_the_camera_refuses_leaves_the_card_where_the_camera_is() {
    let (mut env, backend) = dslr_card();
    backend.taking(false);
    ui(&mut env, "control-set", vec![json!("dslr_white_balance"), json!(3.0)]);
    assert_eq!(env.plugin.tile_indexes()[2], 0, "the model stays on what the camera says, and the card follows it");
    assert_eq!(last_card_props(&env)["tile-indexes"][2], 0);
}
