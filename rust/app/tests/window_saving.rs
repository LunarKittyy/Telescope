// What the window keeps in the settings file besides the plugins' own sections, and how it reports on itself.
mod rig;

use rig::{FakeSource, Listener, Rig};
use serde_json::json;
use std::collections::BTreeMap;
use std::sync::Arc;
use telescope_core::host::{Host, HostShell};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::{SettingsSection, SlintSource, UiSpec};

fn zoomed(name: &str) -> Rig {
    Rig::with(name, |_, heard| {
        let mut panel = Listener::new("panel", heard);
        panel.config = json!({"zoom": 1});
        vec![Box::new(panel) as Box<dyn Plugin>]
    })
}

fn memory(r: &Rig) -> BTreeMap<String, u32> {
    r.w.streams.flow.borrow().slot_memory.borrow().clone()
}

fn asks<R>(r: &Rig, f: impl FnOnce(&mut dyn Host) -> R) -> R {
    r.w.state.borrow_mut().call(0, |_, host| f(host)).unwrap()
}

#[cfg(unix)]
fn lock(r: &Rig, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(r.w.state.borrow().file.path(), std::fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
#[test]
fn a_save_asked_for_while_the_file_cant_be_read_tries_again() {
    let r = zoomed("save-locked");
    r.w.state.borrow_mut().selected = Some("pixel".into());
    r.w.state.borrow_mut().save_now();
    r.configure("panel", json!({"zoom": 2}));
    lock(&r, 0o000);
    asks(&r, |host| host.save_now());
    r.w.process();
    r.pump();
    lock(&r, 0o600);
    let zoom = |r: &Rig| r.w.state.borrow().file.load()["devices"]["pixel"]["plugin_configs"]["panel"]["zoom"].clone();
    assert_eq!(zoom(&r), 1, "not written yet");
    r.skip(600);
    r.pump();
    assert_eq!(zoom(&r), 2);
}

#[test]
fn the_virtual_camera_a_source_went_to_is_saved_and_comes_back() {
    let r = Rig::new("slots-saved");
    r.start();
    r.worker(0).says("ok", "Streaming");
    r.pump();
    r.add("oneplus");
    r.until("the second stream", |r| r.workers_made() == 2);
    assert_eq!(memory(&r), BTreeMap::from([("oneplus".to_string(), 1)]));
    assert!(r.w.state.borrow_mut().save_now());
    assert_eq!(r.w.state.borrow().file.load()["output_slots"], json!({"oneplus": 1}));
    r.w.stop_all_streams();
    r.pump();
    drop(r);

    let again = Rig::new("slots-back");
    again.w.state.borrow().file.save(&mut json!({"output_slots": {"oneplus": 2, "bad": "x"}}));
    again.w.state.borrow_mut().apply_saved_config();
    assert_eq!(memory(&again), BTreeMap::from([("oneplus".to_string(), 2)]));
}

#[test]
fn a_phone_back_with_a_new_id_takes_its_settings_and_virtual_camera() {
    let r = Rig::new("new-id");
    let presets = json!({"presets": {"items": [{"name": "Desk"}]}});
    r.w.state.borrow().file.save(&mut json!({
        "devices": {"old": {"plugin_configs": presets}, "other": {"plugin_configs": {}}},
        "output_slots": {"old": 2}, "selected_device": "old",
    }));
    r.w.state.borrow_mut().apply_saved_config();
    asks(&r, |host| host.move_device_settings("old", "new"));
    let cfg = r.w.state.borrow().file.load();
    assert_eq!(cfg["devices"]["new"]["plugin_configs"], presets);
    assert!(cfg["devices"].get("old").is_none());
    assert_eq!(cfg["output_slots"], json!({"new": 2}));
    assert_eq!(cfg["selected_device"], "new");
    assert_eq!(memory(&r), BTreeMap::from([("new".to_string(), 2)]));
}

#[test]
fn a_browser_keeps_only_the_settings_that_changed() {
    let r = zoomed("changes-only");
    for id in ["browser:guest", "browser:pixel"] {
        r.offer(FakeSource { changes_only: true, ..FakeSource::new(id) });
    }
    let panel = ["panel".to_string()];
    r.w.state.borrow_mut().save_profile(Some("browser:guest"), &panel);
    assert!(!asks(&r, |host| host.has_device_settings("browser:guest")));
    r.configure("panel", json!({"zoom": 2}));
    r.w.state.borrow_mut().save_profile(Some("browser:pixel"), &panel);
    assert_eq!(r.w.state.borrow().file.load()["devices"]["browser:pixel"]["plugin_configs"]["panel"], json!({"zoom": 2}));
    asks(&r, |host| host.forget_device_settings("browser:pixel"));
    assert!(!asks(&r, |host| host.has_device_settings("browser:pixel")));
}

#[test]
fn a_phone_keeps_all_its_settings() {
    let r = zoomed("changes-only-phone");
    r.w.state.borrow_mut().save_profile(Some("pixel"), &["panel".to_string()]);
    assert!(asks(&r, |host| host.has_device_settings("pixel")));
}

#[derive(Default)]
struct Reports;

impl Plugin for Reports {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "connection".into(), ..Default::default() }
    }
    fn diagnostics(&self) -> Vec<(String, String)> {
        vec![("Connection".into(), "usb".into())]
    }
}

struct Broken;

impl Plugin for Broken {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "broken".into(), ..Default::default() }
    }
    fn diagnostics(&self) -> Vec<(String, String)> {
        panic!("gone");
    }
    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        panic!("nope");
    }
}

#[test]
fn diagnostics_report_gathers_plugins_and_recent_status() {
    let r = Rig::with("diagnostics", |_, _| vec![Box::new(Reports) as Box<dyn Plugin>, Box::new(Broken)]);
    r.w.set_status("Can't reach the phone", "err");
    let report = r.w.state.borrow_mut().call(0, |p, host| host.diagnostics_report(&p.diagnostics())).unwrap();
    let lines: Vec<&str> = report.lines().collect();
    assert!(lines.contains(&"Streaming: no"), "{report}");
    assert!(lines.contains(&"Connection: usb"), "{report}");
    assert!(lines.contains(&"broken: couldn't read (panic)"), "{report}");
    assert!(lines.iter().any(|l| l.ends_with("NOTE Status: Can't reach the phone")), "{report}");
}

#[test]
fn a_plugin_that_cant_list_its_settings_doesnt_stop_the_window() {
    let r = Rig::with("no-sections", |_, _| vec![Box::new(Broken) as Box<dyn Plugin>]);
    telescope_shell::settings::open(&r.w, "plugins");
    assert_eq!(telescope_shell::settings::current().unwrap().current_tab(), "plugins");
    telescope_shell::settings::forget();
}

struct Sections;

impl Plugin for Sections {
    fn info(&self) -> PluginInfo {
        PluginInfo { name: "with_sections".into(), ..Default::default() }
    }
    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        const PAGE: &str = "import { VerticalBox } from \"std-widgets.slint\";\nexport component Page inherits VerticalBox { Text { text: \"Mine\"; } }\n";
        let page = |id: &str, tab: &str, title: &str| SettingsSection {
            id: id.into(), tab: tab.into(), ui: UiSpec { source: SlintSource::Text(PAGE.into()), component: "Page".into(), props: json!({}) },
            order: 0, tab_title: title.into(), keywords: String::new(), footer: None,
        };
        vec![page("first", "general", ""), page("second", "my_own", "My own")]
    }
}

#[test]
fn settings_window_collects_sections_from_every_plugin() {
    use slint::ComponentHandle;
    let r = Rig::with("sections", |_, _| vec![Box::new(Sections) as Box<dyn Plugin>]);
    telescope_shell::settings::open(&r.w, "");
    let win = telescope_shell::settings::current().unwrap();
    assert!(win.ui.window().is_visible());
    assert_eq!(win.tabs(), ["general", "my_own", "plugins"], "a tab of its own goes before Plugins");
    telescope_shell::settings::open(&r.w, "my_own");
    assert!(std::rc::Rc::ptr_eq(&win, &telescope_shell::settings::current().unwrap()));
    assert_eq!(win.current_tab(), "my_own");
    telescope_shell::settings::open(&r.w, "nowhere");
    assert_eq!(win.current_tab(), "my_own", "not a tab: stays where it was");
    telescope_shell::settings::forget();
}

#[test]
fn the_gear_button_opens_settings_without_a_menu() {
    use slint::ComponentHandle;
    let r = Rig::new("gear");
    telescope_shell::settings::forget();
    r.w.ui.invoke("settings-clicked", &[]).unwrap();
    r.pump();
    assert!(telescope_shell::settings::current().unwrap().ui.window().is_visible());
    telescope_shell::settings::forget();
}

#[test]
fn under_dev_desktop_a_restart_is_left_to_it() {
    let restarts = |name: &str, streaming: bool| {
        let r = Rig::new(name);
        if streaming {
            r.start();
            r.worker(0).says("ok", "Streaming");
            r.pump();
        }
        assert!(asks(&r, |host| host.restart_app()));
        r.w.process();
        r.pump();
        telescope_shell::window::exit_code()
    };
    // SAFETY: the rig runs one window at a time, and the variable goes before the next
    unsafe {
        std::env::set_var(telescope_core::dev_profile::ENV, telescope_core::testing::scratch_dir("restart-profile"));
        std::env::set_var(telescope_core::dev_profile::SUPERVISED_ENV, "1");
    }
    let (idle, streaming) = (restarts("restart-idle", false), restarts("restart-streaming", true));
    unsafe {
        std::env::remove_var(telescope_core::dev_profile::SUPERVISED_ENV);
        std::env::remove_var(telescope_core::dev_profile::ENV);
    }
    assert_eq!((idle, streaming), (75, 76), "and dev_desktop.py has the next one start streaming");
}

#[test]
fn these_built_in_plugins_keep_their_settings_per_phone() {
    use telescope_effects as fx;
    let per_phone: Vec<Box<dyn Plugin>> = vec![
        Box::new(telescope_shell::plugins::camera_control::CameraControl::new()), Box::new(telescope_outputs::stream_output::StreamOutputPlugin::new(true)),
        Box::new(fx::transforms::TransformsPlugin::new()), Box::new(telescope_setup::monitoring::MonitoringPlugin::new()),
        Box::new(telescope_shell::plugins::presets::PresetsPlugin::new()), Box::new(telescope_audio::plugin::MicrophonePlugin::with_link(Arc::new(rig::FakeLink::default()))),
        Box::new(fx::desk::DeskPlugin::new()),
        Box::new(fx::colour_grading::ColourGradingPlugin::new()), Box::new(fx::fun_pack::FunPackPlugin::new()),
        Box::new(fx::background::BackgroundPlugin::new()), Box::new(fx::chroma_key::ChromaKeyPlugin::new()),
        Box::new(fx::framing::FramingPlugin::new()), Box::new(fx::face_exposure::FaceExposurePlugin::new()), Box::new(fx::name_tag::NameTagPlugin::new()),
    ];
    let names: Vec<String> = per_phone.iter().filter(|p| p.info().device_local).map(|p| p.info().name).collect();
    assert_eq!(names, ["camera_control", "stream_output", "transforms", "monitoring", "presets", "microphone", "desk", "colour_grading", "fun_pack", "background", "chroma_key", "framing", "face_exposure", "name_tag"]);
    let whole_app: Vec<Box<dyn Plugin>> = vec![
        Box::new(fx::cinema::CinemaPlugin::new()), Box::new(fx::privacy_blur::PrivacyBlurPlugin::new()), Box::new(fx::photos::PhotosPlugin::new()),
        Box::new(fx::notes::NotesPlugin::new()), Box::new(fx::qr_reader::QrReaderPlugin::new()),
    ];
    assert!(whole_app.iter().all(|p| !p.info().device_local));
}
