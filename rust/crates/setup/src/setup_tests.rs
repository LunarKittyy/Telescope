//! test_setup.py's tests and test_plugins.py's Setup ones, by the same names.

use super::*;
use std::sync::Mutex;
use telescope_core::frame::{Frame, PixelFormat};
use telescope_core::link::{PairingOffer, PhoneSession, RoutePrefs, Unreachable, UsbDevice};
use telescope_core::media::{Codec, DecodeOptions, Decoder, DecodingStatus, Scaler};
use telescope_core::output::{SetupStatus, Slot, VideoOutput};
use telescope_core::source::PairedPhone;
use telescope_core::testing::FakeHost;

#[derive(Default)]
struct FakeOutputs {
    status: Mutex<SetupStatus>,
    ready: Mutex<Vec<u32>>,
    actions: Mutex<Vec<SetupAction>>,
    /// What set_up answers (ok with "done" when None).
    answer: Mutex<Option<SetupResult>>,
    /// set_up panics, like a driver install that throws.
    panics: bool,
    /// Said as progress before answering.
    progress: Option<String>,
}

impl Outputs for FakeOutputs {
    fn slots(&self) -> Vec<Slot> {
        Vec::new()
    }
    fn formats(&self) -> Vec<PixelFormat> {
        Vec::new()
    }
    fn open(&self, _slot: u32, _w: u32, _h: u32, _fps: u32) -> Result<Box<dyn VideoOutput>, String> {
        Err("no".into())
    }
    fn slot_ready(&self, slot: u32) -> bool {
        slot == 0 || self.ready.lock().unwrap().contains(&slot)
    }
    fn setup_status(&self) -> SetupStatus {
        self.status.lock().unwrap().clone()
    }
    fn set_up(&self, action: SetupAction, progress: &dyn Fn(&str)) -> SetupResult {
        self.actions.lock().unwrap().push(action);
        if self.panics {
            panic!("read-only folder");
        }
        if let Some(say) = &self.progress {
            progress(say);
        }
        self.answer.lock().unwrap().clone().unwrap_or(SetupResult { ok: true, message: "done".into(), command: None })
    }
}

#[derive(Default)]
struct FakeLink {
    adb: Mutex<bool>,
    apk: Mutex<Option<String>>,
    serials: Mutex<Vec<String>>,
    installs: Mutex<Vec<(String, String)>>,
    install_answer: Mutex<Option<String>>,
    adb_answers: Mutex<Vec<Result<String, String>>>,
}

impl Link for FakeLink {
    fn new_identity(&self) -> (String, String) {
        (String::new(), String::new())
    }
    fn resolve(&self, _phone: &PairedPhone, _prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
        Err(Unreachable::NoAnswer)
    }
    fn pair(&self, _id: &str, _name: &str) -> Result<Arc<dyn PairingOffer>, String> {
        Err("no".into())
    }
    fn adb_available(&self) -> bool {
        *self.adb.lock().unwrap()
    }
    fn usb_devices(&self) -> Vec<UsbDevice> {
        self.serials.lock().unwrap().iter().map(|s| UsbDevice { serial: s.clone(), state: "device".into(), model: String::new() }).collect()
    }
    fn bundled_apk(&self) -> Option<String> {
        self.apk.lock().unwrap().clone()
    }
    fn install_app(&self, serial: &str, apk: &str) -> Result<(), String> {
        self.installs.lock().unwrap().push((serial.into(), apk.into()));
        match self.install_answer.lock().unwrap().clone() {
            Some(why) => Err(why),
            None => Ok(()),
        }
    }
    fn get_adb(&self, _own: Option<&str>, _progress: &dyn Fn(&str)) -> Result<String, String> {
        self.adb_answers.lock().unwrap().remove(0)
    }
}

struct NoScaler;

impl Scaler for NoScaler {
    fn rgb(&mut self, _frame: &Frame, _w: u32, _h: u32) -> Vec<u8> {
        Vec::new()
    }
}

#[derive(Default)]
struct FakeMedia {
    status: Mutex<DecodingStatus>,
    looked: Mutex<u32>,
}

impl Media for FakeMedia {
    fn decoder(&self, _codec: Codec, _options: &DecodeOptions) -> Result<Box<dyn Decoder>, String> {
        Err("no".into())
    }
    fn scaler(&self) -> Box<dyn Scaler> {
        Box::new(NoScaler)
    }
    fn decoding_status(&self) -> DecodingStatus {
        *self.looked.lock().unwrap() += 1;
        self.status.lock().unwrap().clone()
    }
}

struct Rig {
    plugin: SetupPlugin,
    host: FakeHost,
    outputs: Arc<FakeOutputs>,
    link: Arc<FakeLink>,
    media: Arc<FakeMedia>,
}

impl Rig {
    fn with(linux: bool, outputs: FakeOutputs) -> Rig {
        let (outputs, link, media) = (Arc::new(outputs), Arc::new(FakeLink::default()), Arc::new(FakeMedia::default()));
        let langs = vec![Language { code: "en".into(), name: "English".into(), draft: false },
                         Language { code: "sv".into(), name: "Svenska".into(), draft: false }];
        // Jobs run where they're started, as _ImmediateThread did
        let plugin = SetupPlugin::new(outputs.clone(), link.clone(), media.clone(), "3.0.0", langs)
            .with_runner(Arc::new(|job| job())).for_platform(linux);
        let mut host = FakeHost::new();
        host.report = "Telescope test report".into();
        let mut r = Rig { plugin, host, outputs, link, media };
        r.plugin.setup(&mut r.host);
        r
    }

    fn linux() -> Rig {
        Rig::with(true, FakeOutputs::default())
    }

    fn windows() -> Rig {
        Rig::with(false, FakeOutputs::default())
    }

    /// The posts the jobs made reach the plugin, as the host thread would hand them over.
    fn deliver(&mut self) {
        loop {
            let posts: Vec<(String, Value)> = std::mem::take(&mut *self.host.posted.lock().unwrap());
            if posts.is_empty() {
                return;
            }
            for (token, data) in posts {
                self.plugin.on_event(&mut self.host, &Event::Posted { owner: NAME.into(), token, data });
            }
        }
    }

    fn ui(&mut self, section: &str, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: section.into(), callback: callback.into(), args });
        self.deliver();
    }

    fn events(&self) -> Vec<Event> {
        self.host.events.clone()
    }

    fn saves(&self) -> usize {
        self.host.calls.iter().filter(|c| *c == "schedule_save").count()
    }

    /// The last request id the host handed out, which its RequestDone answers.
    fn done(&mut self, request: RequestId, ok: bool, message: &str, command: &str) {
        let data = if command.is_empty() { json!({}) } else { json!({"command": command}) };
        self.plugin.on_event(&mut self.host, &Event::RequestDone { request, ok, message: message.into(), data });
    }

    fn pick_custom(&mut self, w: u32, h: u32) {
        self.plugin.panel.canvas_current = canvas::PRESETS.len() - 1;
        self.plugin.panel.custom_w = w;
        self.plugin.panel.custom_h = h;
    }
}

fn status(ready: bool, loaded: bool, persisted: bool) -> SetupStatus {
    SetupStatus { ready, loaded, persisted, ..SetupStatus::default() }
}

#[test]
fn dialog_preset_visibility_and_apply_callback() {
    let mut r = Rig::linux();
    r.plugin.set_config(&json!({"canvas_preset": "Custom...", "custom_canvas_w": 1000, "custom_canvas_h": 700})).unwrap();
    r.plugin.sync_panel();
    assert_eq!(r.plugin.video_props()["custom-shown"], json!(true));
    r.ui("video", "apply-clicked", vec![]);
    assert!(r.host.calls.contains(&"restart_vcam_canvas 1000x700".to_string()));
    assert_eq!(r.plugin.video_props()["apply-enabled"], json!(false));
    assert_eq!(r.plugin.video_props()["canvas-status-shown"], json!(true));
    r.ui("video", "canvas-picked", vec![json!(1)]);
    assert_eq!(r.plugin.video_props()["custom-shown"], json!(false));
}

#[test]
fn v4l_status_states() {
    for (ready, loaded, text, kind) in [(true, false, "Ready:", "status_ok"), (false, true, "another config", "status_warn"),
                                        (false, false, "Not loaded", "status_err")] {
        let mut r = Rig::linux();
        *r.outputs.status.lock().unwrap() = status(ready, loaded, false);
        r.ui("virtual-camera", "check-clicked", vec![]);
        assert!(r.plugin.panel.v4l.text.contains(text), "{}", r.plugin.panel.v4l.text);
        assert_eq!(r.plugin.panel.v4l.kind, kind);
    }
}

#[test]
fn v4l_async_load_unload_and_results() {
    let mut r = Rig::linux();
    *r.outputs.answer.lock().unwrap() = Some(SetupResult { ok: true, message: "loaded".into(), command: None });
    r.ui("virtual-camera", "load-clicked", vec![]);
    assert_eq!(r.plugin.panel.v4l, Line::new("Loaded - loaded", "status_ok"));
    *r.outputs.answer.lock().unwrap() = Some(SetupResult { ok: false, message: "busy".into(), command: None });
    r.ui("virtual-camera", "unload-clicked", vec![]);
    assert_eq!(r.plugin.panel.v4l, Line::new("Failed - busy", "status_err"));
    assert_eq!(*r.outputs.actions.lock().unwrap(), vec![SetupAction::Load { persist: false }, SetupAction::Unload]);
}

#[test]
fn refresh_persist_status() {
    for persisted in [true, false] {
        let mut r = Rig::linux();
        *r.outputs.status.lock().unwrap() = status(true, true, persisted);
        r.plugin.v4l_check(&mut r.host);
        r.deliver();
        assert_eq!(r.plugin.panel.persist_checked, persisted);
    }
}

#[test]
fn persist_toggle_runs_correct_action_and_displays_success() {
    let mut r = Rig::linux();
    *r.outputs.answer.lock().unwrap() = Some(SetupResult { ok: true, message: "enabled".into(), command: None });
    r.ui("virtual-camera", "persist-toggled", vec![json!(true)]);
    assert!(r.plugin.panel.persist_enabled);
    assert_eq!(r.plugin.panel.persist, Some(Line::new("enabled", "status_ok")));
    r.ui("virtual-camera", "persist-toggled", vec![json!(false)]);
    assert_eq!(*r.outputs.actions.lock().unwrap(), vec![SetupAction::Persist { on: true }, SetupAction::Persist { on: false }]);
}

#[test]
fn failed_persist_action_reverts_checkbox() {
    let mut r = Rig::linux();
    r.plugin.panel.persist_checked = true;
    r.plugin.on_persist_result(false, "denied", "");
    assert!(!r.plugin.panel.persist_checked);
    assert_eq!(r.plugin.panel.persist, Some(Line::new("denied", "status_err")));
}

#[test]
fn windows_setup_status() {
    for (uc, adb, uc_text, button, adb_text) in [
        ("Telescope", true, "Ready", "Reinstall", "Ready"),
        ("", false, "Not installed", "Install driver", "Not found. Pairing and installing over USB need it; Wi-Fi doesn't."),
    ] {
        let mut r = Rig::windows();
        r.plugin.on_win_checks(uc, adb, false, "", true); // the DLLs are there
        assert_eq!(r.plugin.panel.uc.text, uc_text);
        assert_eq!(r.plugin.panel.uc_button, button);
        assert_eq!(r.plugin.panel.adb.text, adb_text);
    }
}

#[test]
fn windows_status_offers_download_when_dll_missing() {
    let mut r = Rig::windows();
    r.plugin.on_win_checks("", true, false, "", false);
    assert_eq!(r.plugin.panel.uc_button, "Install driver");
    assert!(r.plugin.panel.uc.text.contains("downloads"));
}

#[test]
fn windows_background_check_emits_current_status() {
    // Windows 10 with UnityCapture registered as Telescope, and no adb
    let mut r = Rig::with(false, FakeOutputs { status: Mutex::new(SetupStatus { ready: true, loaded: true, registered_name: "Telescope".into(),
                                                                                ..SetupStatus::default() }), ..FakeOutputs::default() });
    r.plugin.check_win_setup(&mut r.host);
    r.deliver();
    assert_eq!(r.plugin.panel.uc.text, "Ready");
    assert!(r.plugin.panel.adb.text.contains("Not found"));
}

#[test]
fn a_camera_that_isnt_telescopes_is_not_shown_as_ready() {
    // Something is registered and loaded, but not as Telescope's own UnityCapture: no name to show
    let mut r = Rig::with(false, FakeOutputs { status: Mutex::new(SetupStatus { ready: true, loaded: true, ..SetupStatus::default() }),
                                               ..FakeOutputs::default() });
    r.plugin.check_win_setup(&mut r.host);
    r.deliver();
    assert!(r.plugin.panel.uc.text.starts_with("Not installed"), "{}", r.plugin.panel.uc.text);
    assert_eq!(r.plugin.panel.uc_button, "Install driver");
}

#[test]
fn without_adb_advanced_offers_to_get_it() {
    let mut r = Rig::windows();
    r.plugin.on_win_checks("Telescope", true, false, "", true);
    assert!(!r.plugin.panel.adb_row);
    r.plugin.on_win_checks("Telescope", false, false, "", true);
    assert!(r.plugin.panel.adb_row);
    r.link.adb_answers.lock().unwrap().extend([Err("Couldn't download adb: <urlopen error x>".into()), Ok("37.0.1".into())]);
    r.ui("virtual-camera", "adb-clicked", vec![]);
    assert_eq!(r.plugin.panel.adb.text, "Couldn't download adb: <urlopen error x>");
    assert!(r.plugin.panel.adb_button == "Try again" && r.plugin.panel.adb_enabled);
    r.ui("virtual-camera", "adb-clicked", vec![]);
    assert_eq!(r.plugin.panel.adb.text, "Ready (adb 37.0.1, from Google)");
    assert!(!r.plugin.panel.adb_row);
}

#[test]
fn an_old_registration_offers_the_rename() {
    let mut r = Rig::windows();
    r.plugin.on_win_checks("Unity Video Capture", true, false, "", true);
    assert_eq!(r.plugin.panel.uc_button, "Rename");
    assert!(r.plugin.panel.uc.text.contains("Unity Video Capture"));
}

#[test]
fn a_registration_in_the_app_folder_asks_for_a_reinstall_and_says_why() {
    let mut r = Rig::windows();
    r.plugin.on_win_checks("Telescope", true, true, "", true);
    assert_eq!(r.plugin.panel.uc_button, "Reinstall");
    assert!(r.plugin.panel.uc.text.contains("security"));
    assert!(r.plugin.panel.uc_tip.contains("Program Files"));
    r.plugin.on_uc_done(true, "Installed");
    assert!(r.plugin.panel.uc.text == "Ready" && r.plugin.panel.uc_tip.is_empty());
}

#[test]
fn install_driver_shows_progress_then_ready() {
    let mut r = Rig::with(false, FakeOutputs { progress: Some("Registering (admin access required)...".into()), ..FakeOutputs::default() });
    r.plugin.install_uc(&mut r.host);
    let posts = r.host.posted.lock().unwrap().clone();
    assert_eq!(posts[0], ("uc-msg".to_string(), json!("Registering (admin access required)...")));
    r.deliver();
    assert_eq!(r.plugin.panel.uc.text, "Ready");
    assert_eq!(r.plugin.panel.uc_button, "Reinstall");
}

#[test]
fn reinstalling_the_driver_keeps_the_extra_cameras() {
    for (ready, devices) in [(vec![], 1), (vec![1, 2, 3], 4)] {
        let mut r = Rig::with(false, FakeOutputs { ready: Mutex::new(ready), ..FakeOutputs::default() });
        r.plugin.install_uc(&mut r.host);
        assert_eq!(*r.outputs.actions.lock().unwrap(), vec![SetupAction::Install { devices, reset: false }]);
    }
}

#[test]
fn install_driver_failure_offers_a_retry() {
    let mut r = Rig::windows();
    *r.outputs.answer.lock().unwrap() = Some(SetupResult { ok: false, message: "offline".into(), command: None });
    r.ui("virtual-camera", "uc-clicked", vec![]);
    assert_eq!(r.plugin.panel.uc.text, "Failed: offline");
    assert_eq!(r.plugin.panel.uc_button, "Retry");
}

#[test]
fn the_windows_11_camera_installed_is_ready() {
    let mut r = Rig::windows();
    r.plugin.on_win_checks("", true, false, "ready", false);
    assert_eq!(r.plugin.panel.uc.text, "Ready");
    assert_eq!(r.plugin.panel.uc_button, "Reinstall");
}

#[test]
fn windows_11_offers_its_camera_over_an_older_one() {
    for (uc, mf, text) in [("Telescope", "offer", "Some apps can't see it"), ("", "update", "Update ready")] {
        let mut r = Rig::windows();
        r.plugin.on_win_checks(uc, true, false, mf, false);
        assert_eq!(r.plugin.panel.uc.text, text);
        assert_eq!(r.plugin.panel.uc_button, "Update");
        assert!(r.plugin.panel.uc_tip.contains("Windows 11"));
    }
}

#[test]
fn windows_11_with_nothing_installed_has_nothing_to_download() {
    let mut r = Rig::windows();
    r.plugin.on_win_checks("", true, false, "offer", false);
    assert_eq!(r.plugin.panel.uc.text, "Not installed");
    assert_eq!(r.plugin.panel.uc_button, "Install driver");
}

#[test]
fn apk_install_rejects_missing_adb_or_device() {
    let mut r = Rig::linux();
    r.ui("phone-app", "apk-clicked", vec![]);
    assert!(r.plugin.panel.apk.text.contains("needs adb"));
    *r.link.adb.lock().unwrap() = true;
    *r.link.apk.lock().unwrap() = Some("/tmp/Telescope.apk".into());
    r.ui("phone-app", "apk-clicked", vec![]);
    assert!(r.plugin.panel.apk.text.contains("No phone found over USB"));
}

#[test]
fn apk_install_uses_selected_device_and_reports_success() {
    let mut r = Rig::linux();
    *r.link.adb.lock().unwrap() = true;
    *r.link.apk.lock().unwrap() = Some("/tmp/Telescope.apk".into());
    *r.link.serials.lock().unwrap() = vec!["a".into(), "b".into()];
    r.ui("phone-app", "apk-clicked", vec![]);
    assert_eq!(r.plugin.apk_props()["pick-shown"], json!(true));
    r.ui("phone-app", "pick-done", vec![json!(1)]);
    assert_eq!(*r.link.installs.lock().unwrap(), vec![("b".to_string(), "/tmp/Telescope.apk".to_string())]);
    assert_eq!(r.plugin.panel.apk, Line::new("Installed", "status_ok"));
}

#[test]
fn apk_install_cancel_and_failure_detail() {
    let mut r = Rig::linux();
    *r.link.adb.lock().unwrap() = true;
    *r.link.apk.lock().unwrap() = Some("/tmp/Telescope.apk".into());
    *r.link.serials.lock().unwrap() = vec!["a".into(), "b".into()];
    r.ui("phone-app", "apk-clicked", vec![]);
    r.ui("phone-app", "pick-cancelled", vec![]);
    assert!(r.plugin.panel.apk_enabled && r.link.installs.lock().unwrap().is_empty());

    *r.link.serials.lock().unwrap() = vec!["a".into()];
    *r.link.install_answer.lock().unwrap() = Some("Failure [bad apk]".into());
    r.ui("phone-app", "apk-clicked", vec![]);
    assert_eq!(r.plugin.panel.apk, Line::new("Failure [bad apk]", "status_err"));
}

#[test]
fn apk_picker_cancel_and_selected_file() {
    let mut r = Rig::linux();
    *r.link.adb.lock().unwrap() = true;
    let initial = r.plugin.panel.apk.clone();
    r.ui("phone-app", "apk-clicked", vec![]);
    assert!(r.host.calls.iter().any(|c| c == "pick_file Open Select APK"));
    let first = r.plugin.apk_pick.unwrap();
    r.done(first, false, "", ""); // cancelled
    assert_eq!(r.plugin.panel.apk, initial);

    *r.link.serials.lock().unwrap() = vec!["phone".into()];
    *r.link.install_answer.lock().unwrap() = Some("adb install failed".into());
    r.ui("phone-app", "apk-clicked", vec![]);
    let second = r.plugin.apk_pick.unwrap();
    r.plugin.on_event(&mut r.host, &Event::RequestDone { request: second, ok: true, message: String::new(),
                                                         data: json!({"path": "/x/chosen.apk", "filter": "Android Package (*.apk)"}) });
    r.deliver();
    assert_eq!(*r.link.installs.lock().unwrap(), vec![("phone".to_string(), "/x/chosen.apk".to_string())]);
    assert_eq!(r.plugin.panel.apk.text, "adb install failed");
}

#[test]
fn setup_plugin_builds_its_cards_and_syncs_config() {
    let mut r = Rig::linux();
    assert!(r.plugin.card().is_none());
    r.plugin.set_config(&json!({"canvas_preset": "Custom...", "custom_canvas_w": 900, "custom_canvas_h": 600})).unwrap();
    let sections = r.plugin.settings_sections();
    let video = sections.iter().find(|s| s.id == "video").unwrap();
    assert_eq!(video.ui.props["canvas-current"], json!(canvas::PRESETS.len() - 1));
    assert_eq!(video.ui.props["custom-w-text"], json!("900 px"));
    r.plugin.set_config(&json!({"canvas_preset": "720p 16:9 - 1280 x 720"})).unwrap();
    r.ui("video", "shown", vec![]); // the saved value changed while Settings was closed
    assert_eq!(r.plugin.panel.canvas_current, 3);
    r.ui("about", "diag-clicked", vec![]);
    assert_eq!(r.host.clipboard.as_deref(), Some("Telescope test report"));
    assert_eq!(r.plugin.about_props()["diag-text"], json!("Copied"));
    r.plugin.on_event(&mut r.host, &Event::TimerFired { owner: NAME.into(), token: "copied".into() });
    assert_eq!(r.plugin.about_props()["diag-text"], json!("Copy diagnostics"));
    r.host.clipboard = None;
    r.ui("about", "bug-clicked", vec![]);
    assert_eq!(r.host.clipboard.as_deref(), Some("Telescope test report"));
    assert!(r.host.calls.contains(&format!("open_url {BUG_REPORT_URL}")));
}

#[cfg(unix)]
#[test]
fn open_log_shows_once_there_is_a_log_and_opens_it_with_any_name() {
    let mut r = Rig::linux();
    assert_eq!(r.plugin.about_props()["log-shown"], json!(false));
    r.plugin.log_path = Some("/tmp/telescope-me #1/telescope.log".into());
    assert_eq!(r.plugin.about_props()["log-shown"], json!(true));
    r.ui("about", "log-clicked", vec![]);
    assert!(r.host.calls.contains(&"open_url file:///tmp/telescope-me%20%231/telescope.log".to_string()), "{:?}", r.host.calls);
}

#[test]
fn show_event_refreshes_platform_specific_status() {
    let mut r = Rig::linux();
    r.ui("devices", "shown", vec![]);
    assert!(r.host.calls.iter().any(|c| c.starts_with("set_ui_props virtual-camera")));
    assert!(r.plugin.panel.v4l.text.contains("Not loaded"), "looked at the module");
    let mut w = Rig::with(false, FakeOutputs { status: Mutex::new(SetupStatus { windows_camera: true, ..SetupStatus::default() }),
                                               ..FakeOutputs::default() });
    w.ui("devices", "shown", vec![]);
    assert_eq!(w.plugin.panel.uc.text, "Ready", "looked at the Windows camera");
}

#[test]
fn setup_contributes_settings_sections_instead_of_a_panel() {
    let mut r = Rig::linux();
    assert!(r.plugin.card().is_none());
    let mut tabs: Vec<String> = r.plugin.settings_sections().into_iter().map(|s| s.tab).collect();
    tabs.sort();
    tabs.dedup();
    assert_eq!(tabs, ["about", "audio", "devices", "general", "video"]);
}

#[test]
fn max_zoom_loads_with_a_safe_fallback_and_is_announced() {
    for (stored, want) in [(None, 10), (Some(json!(15)), 15), (Some(json!(1)), 10), (Some(json!(99)), 10), (Some(json!("8")), 10),
                           (Some(json!(true)), 10)] {
        let mut r = Rig::linux();
        r.plugin.set_config(&stored.map_or(json!({}), |v| json!({"max_zoom": v}))).unwrap();
        r.plugin.on_plugins_ready(&mut r.host);
        assert_eq!(r.plugin.get_config()["max_zoom"], json!(want));
        let seen: Vec<_> = r.events().into_iter().filter(|e| matches!(e, Event::MaxZoomChanged { .. })).collect();
        assert_eq!(seen, vec![Event::MaxZoomChanged { max_zoom: want }]);
    }
}

#[test]
fn max_zoom_spinbox_applies_straight_away() {
    let mut r = Rig::linux();
    r.plugin.set_config(&json!({"max_zoom": 12})).unwrap();
    r.plugin.on_plugins_ready(&mut r.host);
    assert_eq!(r.plugin.settings_sections()[1].ui.props["zoom-text"], json!("12×"));
    r.ui("video", "zoom-committed", vec![json!("20×")]);
    let seen: Vec<_> = r.events().into_iter().filter(|e| matches!(e, Event::MaxZoomChanged { .. })).collect();
    assert_eq!(seen, vec![Event::MaxZoomChanged { max_zoom: 12 }, Event::MaxZoomChanged { max_zoom: 20 }]);
    assert_eq!(r.plugin.get_config()["max_zoom"], json!(20));
    assert!(r.saves() >= 1);
}

#[test]
fn max_gain_loads_with_a_safe_fallback_and_is_announced() {
    for (stored, want) in [(None, 12), (Some(json!(48)), 48), (Some(json!(24)), 24), (Some(json!(30)), 12), (Some(json!("36")), 12),
                           (Some(json!(true)), 12)] {
        let mut r = Rig::linux();
        r.plugin.set_config(&stored.map_or(json!({}), |v| json!({"max_gain": v}))).unwrap();
        r.plugin.on_plugins_ready(&mut r.host);
        assert_eq!(r.plugin.get_config()["max_gain"], json!(want));
        let seen: Vec<_> = r.events().into_iter().filter(|e| matches!(e, Event::MaxGainChanged { .. })).collect();
        assert_eq!(seen, vec![Event::MaxGainChanged { max_gain: want }]);
    }
}

#[test]
fn max_gain_buttons_apply_straight_away() {
    let mut r = Rig::linux();
    r.plugin.set_config(&json!({"max_gain": 36})).unwrap();
    r.plugin.on_plugins_ready(&mut r.host);
    assert_eq!(r.plugin.mic_props()["gain-current"], json!(2));
    r.ui("microphone", "gain-picked", vec![json!(3)]);
    let seen: Vec<_> = r.events().into_iter().filter(|e| matches!(e, Event::MaxGainChanged { .. })).collect();
    assert_eq!(seen, vec![Event::MaxGainChanged { max_gain: 36 }, Event::MaxGainChanged { max_gain: 48 }]);
    assert!(r.plugin.get_config()["max_gain"] == json!(48) && r.saves() >= 1);
}

#[test]
fn a_driver_install_that_crashes_still_frees_the_button() {
    let mut r = Rig::with(false, FakeOutputs { panics: true, ..FakeOutputs::default() });
    r.ui("virtual-camera", "uc-clicked", vec![]);
    assert!(r.plugin.panel.uc_enabled);
    assert!(r.plugin.panel.uc.text.contains("read-only folder"), "{}", r.plugin.panel.uc.text);
}

#[test]
fn limiter_defaults_on_and_is_announced() {
    for (stored, want) in [(None, true), (Some(json!(false)), false), (Some(json!(true)), true), (Some(json!("off")), true)] {
        let mut r = Rig::linux();
        r.plugin.set_config(&stored.map_or(json!({}), |v| json!({"limiter": v}))).unwrap();
        r.plugin.on_plugins_ready(&mut r.host);
        assert_eq!(r.plugin.get_config()["limiter"], json!(want));
        let seen: Vec<_> = r.events().into_iter().filter(|e| matches!(e, Event::LimiterChanged { .. })).collect();
        assert_eq!(seen, vec![Event::LimiterChanged { on: want }]);
    }
}

#[test]
fn limiter_switch_applies_straight_away() {
    let mut r = Rig::linux();
    r.plugin.set_config(&json!({})).unwrap();
    r.plugin.on_plugins_ready(&mut r.host);
    assert_eq!(r.plugin.mic_props()["limiter-checked"], json!(true));
    r.ui("microphone", "limiter-toggled", vec![json!(false)]);
    let seen: Vec<_> = r.events().into_iter().filter(|e| matches!(e, Event::LimiterChanged { .. })).collect();
    assert_eq!(seen, vec![Event::LimiterChanged { on: true }, Event::LimiterChanged { on: false }]);
    assert!(r.plugin.get_config()["limiter"] == json!(false) && r.saves() >= 1);
}

#[test]
fn extra_cameras_show_up_and_can_be_removed() {
    let mut r = Rig::with(true, FakeOutputs { ready: Mutex::new(vec![1, 2]), ..FakeOutputs::default() });
    r.ui("devices", "shown", vec![]);
    assert!(r.plugin.panel.extras_shown && r.plugin.panel.extras_enabled);
    assert_eq!(r.plugin.panel.extras.text, "Telescope 2, Telescope 3");

    r.ui("virtual-camera", "extras-clicked", vec![]);
    assert!(r.host.calls.contains(&"remove_extra_cameras".to_string()));
    r.outputs.ready.lock().unwrap().clear();
    let request = r.plugin.extras_request.unwrap();
    r.done(request, true, "", "");
    assert_eq!(r.plugin.panel.extras.text, "Removed");
    assert!(r.plugin.panel.extras_shown && !r.plugin.panel.extras_button);

    r.host.streaming = true;
    r.host.streams = Some(2);
    *r.outputs.ready.lock().unwrap() = vec![1];
    r.ui("devices", "shown", vec![]);
    assert!(!r.plugin.panel.extras_enabled); // a stream still goes to one
}

#[test]
fn the_graphics_card_row_waits_for_settings_and_fills_in_after() {
    let mut r = Rig::linux();
    r.media.status.lock().unwrap().hardware_device = Some("cuda".into());
    assert!(*r.media.looked.lock().unwrap() == 0 && !r.plugin.panel.hw_enabled);
    r.plugin.look_for_graphics_card(&mut r.host);
    r.plugin.look_for_graphics_card(&mut r.host);
    r.deliver();
    assert!(*r.media.looked.lock().unwrap() == 1 && r.plugin.panel.hw_enabled);
    assert!(r.plugin.panel.hw_line.contains("NVIDIA CUDA"));
}

// test_plugins.py

#[test]
fn setup_plugin_canvas_config() {
    for (cfg, want) in [
        (json!({}), (None, None)),
        (json!({"canvas_preset": "1280 × 720"}), (None, None)),
        (json!({"canvas_preset": "720p 16:9 - 1280 x 720"}), (Some(1280), Some(720))),
        (json!({"canvas_preset": "Custom...", "custom_canvas_w": 1111, "custom_canvas_h": 777}), (Some(1112), Some(778))),
        (json!({"canvas_preset": "Custom...", "custom_canvas_w": 0, "custom_canvas_h": 720}), (Some(1920), Some(1080))),
    ] {
        let mut r = Rig::linux();
        r.plugin.set_config(&cfg).unwrap();
        assert_eq!(r.plugin.canvas_dims(), want, "{cfg}");
    }
}

#[test]
fn setup_plugin_config_round_trip() {
    let mut r = Rig::linux();
    let cfg = json!({"canvas_preset": "Custom...", "custom_canvas_w": 2048, "custom_canvas_h": 1536, "max_zoom": 15, "max_gain": 36,
                     "limiter": false, "hw_decode": true, "power_saving": true, "plugin_hues": false, "language": "sv"});
    r.plugin.set_config(&cfg).unwrap();
    assert_eq!(r.plugin.get_config(), cfg);
    r.plugin.on_plugins_ready(&mut r.host);
    assert_eq!(r.host.plugin_hues, Some(false));
}

#[test]
fn hardware_decoding_is_off_unless_turned_on() {
    let mut r = Rig::linux();
    r.plugin.set_config(&json!({"hw_decode": "yes"})).unwrap();
    assert_eq!(r.plugin.get_config()["hw_decode"], json!(false));
    r.ui("video", "hw-toggled", vec![json!(true)]);
    assert_eq!(r.plugin.get_config()["hw_decode"], json!(true));
    assert_eq!(r.plugin.diagnostics(), vec![("Decoding".to_string(), "not streaming (graphics card selected)".to_string())]);
    r.media.status.lock().unwrap().backends = vec!["processor".into()];
    r.media.status.lock().unwrap().hardware_failed = true;
    assert_eq!(r.plugin.diagnostics()[0].1, "processor (graphics card failed; processor until restart)");
}

#[test]
fn power_saving_effects_are_off_unless_turned_on() {
    let mut r = Rig::linux();
    r.plugin.set_config(&json!({"power_saving": "yes"})).unwrap();
    assert_eq!(r.plugin.get_config()["power_saving"], json!(false));
    r.plugin.on_plugins_ready(&mut r.host);
    assert_eq!(r.host.power_saving, Some(false));
    r.ui("video", "saving-toggled", vec![json!(true)]);
    assert_eq!(r.plugin.get_config()["power_saving"], json!(true));
    assert_eq!(r.host.power_saving, Some(true));
}

#[test]
fn setup_plugin_apply_canvas_persists_and_reports_result() {
    let mut r = Rig::linux();
    r.ui("video", "shown", vec![]);
    r.pick_custom(900, 700);
    r.ui("video", "apply-clicked", vec![]);
    assert_eq!(r.plugin.canvas_dims(), (Some(900), Some(700)));
    assert!(r.host.calls.contains(&"restart_vcam_canvas 900x700".to_string()));
    let (request, _) = r.plugin.canvas_request.clone().unwrap();
    let before = r.saves();
    r.done(request, true, "done", "");
    assert_eq!(r.saves(), before + 1);
    assert_eq!(r.plugin.panel.canvas_status, Some(Line::new("Done - loopback reloaded.", "status_ok")));
}

#[test]
fn setup_plugin_keeps_a_canvas_the_host_refused() {
    let mut r = Rig::linux();
    r.plugin.set_config(&json!({"canvas_preset": "Custom...", "custom_canvas_w": 1920, "custom_canvas_h": 1080})).unwrap();
    r.pick_custom(1280, 720);
    r.ui("video", "apply-clicked", vec![]);
    let (request, _) = r.plugin.canvas_request.clone().unwrap();
    r.done(request, false, "Stop all but one camera first", "");
    assert_eq!(r.plugin.canvas_dims(), (Some(1920), Some(1080)));
    assert_eq!(r.plugin.get_config()["custom_canvas_w"], json!(1920));
    assert_eq!(r.plugin.panel.canvas_status, Some(Line::new("Failed: Stop all but one camera first", "status_err")));
}

#[test]
fn setup_plugin_shows_a_command_to_run_where_it_was_asked_for() {
    let mut r = Rig::linux();
    r.ui("video", "shown", vec![]);
    r.plugin.panel.canvas_current = 3;
    r.ui("video", "apply-clicked", vec![]);
    let (request, _) = r.plugin.canvas_request.clone().unwrap();
    r.done(request, false, "Run this", "sudo modprobe v4l2loopback");
    assert_eq!(r.plugin.panel.canvas_command, "sudo modprobe v4l2loopback");
    assert!(r.host.issues.is_empty());
    r.ui("about", "shown", vec![]); // moved on while it ran: the main window says it instead
    r.ui("video", "apply-clicked", vec![]);
    let (request, _) = r.plugin.canvas_request.clone().unwrap();
    r.done(request, false, "Run this", "sudo modprobe v4l2loopback");
    assert_eq!(r.host.issues.iter().map(|(_, i)| i.details.clone()).collect::<Vec<_>>(), ["sudo modprobe v4l2loopback"]);
}

#[test]
fn setup_plugin_uses_the_new_canvas_for_the_restart_it_asks_for() {
    let mut r = Rig::linux();
    r.plugin.panel.canvas_current = 3;
    r.ui("video", "apply-clicked", vec![]);
    // The window reads the canvas from these settings for the restart, so it's in already
    assert_eq!(r.plugin.canvas_dims(), (Some(1280), Some(720)));
    let (request, _) = r.plugin.canvas_request.clone().unwrap();
    r.done(request, true, "", "");
    assert_eq!(r.plugin.canvas_dims(), (Some(1280), Some(720)));
}

#[test]
fn setup_dialog_rounds_an_odd_custom_size_up_to_even() {
    let mut r = Rig::linux();
    r.pick_custom(1921, 1081);
    assert_eq!(r.plugin.selected_dims(), (Some(1922), Some(1082)));
    assert_eq!((r.plugin.panel.custom_w, r.plugin.panel.custom_h), (1922, 1082));
    // The arrows step by 2
    r.ui("video", "custom-step", vec![json!("w"), json!(1)]);
    assert_eq!(r.plugin.panel.custom_w, 1924);
}

#[test]
fn setup_dialog_canvas_dimension_selection_and_result_messages() {
    let mut r = Rig::linux();
    r.pick_custom(1234, 568);
    assert_eq!(r.plugin.selected_dims(), (Some(1234), Some(568)));
    r.plugin.panel.canvas_current = 0;
    assert_eq!(r.plugin.selected_dims(), (None, None));
    r.plugin.set_canvas_apply_result(false, "device in use", "");
    assert!(r.plugin.panel.canvas_status.clone().unwrap().text.contains("Close OBS"));
    r.plugin.set_canvas_apply_result(false, "permission denied", "");
    assert_eq!(r.plugin.panel.canvas_status.clone().unwrap().text, "Failed: permission denied");
    r.plugin.set_canvas_apply_result(true, "ok", "");
    assert!(r.plugin.panel.canvas_status.clone().unwrap().text.contains("Done"));
}

#[test]
fn the_card_colours_and_language_apply_and_save() {
    let mut r = Rig::linux();
    r.ui("look", "hues-picked", vec![json!(1)]);
    assert_eq!(r.host.plugin_hues, Some(false));
    assert_eq!(r.plugin.get_config()["plugin_hues"], json!(false));
    r.ui("look", "lang-picked", vec![json!(2)]);
    assert_eq!(r.plugin.get_config()["language"], json!("sv"));
    assert_eq!(r.plugin.look_props()["lang-restart"], json!(true));
    r.ui("look", "restart-clicked", vec![]);
    assert!(r.host.calls.contains(&"restart_app".to_string()));
}

#[test]
fn the_credit_link_splits_around_its_name() {
    assert_eq!(split_link("Made by <a href=\"{url}\">LunarKittyy</a>"), ("Made by ".into(), "LunarKittyy".into(), String::new()));
    assert_eq!(split_link("Von <a href=\"x\">LunarKittyy</a> gemacht"), ("Von ".into(), "LunarKittyy".into(), " gemacht".into()));
}
