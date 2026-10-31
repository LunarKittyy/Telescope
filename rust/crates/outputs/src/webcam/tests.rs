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
    assert!(env.host.calls.iter().any(|c| c == "show_issue webcam:usb-c920 Can't open HD Pro Webcam C920"));
    assert_eq!(status(&env), "In use by zoom");
    report(&mut env, "usb-c920", "ok", "1280×720, MJPEG, 30 fps");
    assert!(env.host.calls.iter().any(|c| c == "clear_issue Some(\"webcam:usb-c920\")"));
    assert_eq!(status(&env), "1280×720, MJPEG, 30 fps");
}

// test_webcam_plugin.py::test_a_busy_webcam_without_a_known_app_still_says_what_to_do
#[test]
fn a_busy_webcam_without_a_known_app_still_says_what_to_do() {
    let mut env = cams();
    stream(&mut env, "usb-c920");
    report(&mut env, "usb-c920", "busy", "");
    assert!(env.host.calls.iter().any(|c| c.starts_with("show_issue webcam:usb-c920")));
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
