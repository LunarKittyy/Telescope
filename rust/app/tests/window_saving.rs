// What the window keeps in the settings file besides the plugins' own sections, and how it reports on itself.
mod rig;

use rig::{FakeSource, Listener, Rig};
use serde_json::json;
use std::collections::BTreeMap;
use telescope_core::host::{Host, HostShell};
use telescope_core::plugin::{Plugin, PluginInfo};
use telescope_core::ui::SettingsSection;

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
