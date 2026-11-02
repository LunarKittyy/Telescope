// Per-phone settings through the real window with the real Connection and Camera plugins (test_connection.py's
// window_with_plugins tests).
mod rig;

use rig::Rig;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use telescope_core::event::Event;
use telescope_core::link::{Link, PairingOffer, PhoneSession, RoutePrefs, Unreachable};
use telescope_core::plugin::Plugin;
use telescope_core::source::PairedPhone;
use telescope_core::ui::UiEvent;
use telescope_link::connection::Tunnels;
use telescope_link::plugin::ConnectionPlugin;
use telescope_link::shared::Shared;
use telescope_shell::plugins::camera_control::CameraControl;

struct NoForwards;

impl Tunnels for NoForwards {
    fn acquire(&self, _serial: &str, _remote_port: u16) -> Option<u16> {
        None
    }
    fn release(&self, _serial: &str, _remote_port: u16) {}
}

/// A link whose next pairing offer brings this phone.
#[derive(Default)]
struct Pairs {
    next: Mutex<Option<PairedPhone>>,
}

struct Offer(Option<PairedPhone>, Mutex<bool>);

impl PairingOffer for Offer {
    fn payload(&self) -> String {
        "{}".into()
    }
    fn port(&self) -> u16 {
        8765
    }
    fn addresses(&self) -> Vec<String> {
        Vec::new()
    }
    fn offer_over_usb(&self, _package: &str) -> usize {
        0
    }
    fn poll(&self) -> Option<PairedPhone> {
        let mut taken = self.1.lock().unwrap();
        (!std::mem::replace(&mut *taken, true)).then(|| self.0.clone()).flatten()
    }
}

impl Link for Pairs {
    fn new_identity(&self) -> (String, String) {
        ("computer".into(), "Desk".into())
    }
    fn resolve(&self, _phone: &PairedPhone, _prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
        Err(Unreachable::NoAnswer)
    }
    fn pair(&self, _computer_id: &str, _computer_name: &str) -> Result<Arc<dyn PairingOffer>, String> {
        Ok(Arc::new(Offer(self.next.lock().unwrap().take(), Mutex::new(false))))
    }
}

fn window(name: &str) -> (Rig, Arc<Pairs>, Arc<Shared>) {
    let link = Arc::new(Pairs::default());
    let shared = Shared::new(Arc::new(NoForwards));
    let (l, s) = (link.clone(), shared.clone());
    let rig = Rig::with(name, move |_, _| {
        vec![Box::new(ConnectionPlugin::new(l, s)) as Box<dyn Plugin>, Box::new(CameraControl::new())]
    });
    (rig, link, shared)
}

fn to_connection(r: &Rig, f: impl FnOnce(&mut dyn Plugin, &mut dyn telescope_core::host::Host)) {
    let mut state = r.w.state.borrow_mut();
    let at = state.index("connection").unwrap();
    state.call(at, |p, host| f(p, host)).unwrap();
    drop(state);
    r.w.process();
    r.pump();
}

fn ui(r: &Rig, which: &str, callback: &str, args: Vec<Value>) {
    let event = UiEvent { ui: which.into(), callback: callback.into(), args };
    to_connection(r, |p, host| p.on_ui(host, &event));
}

fn pair(r: &Rig, link: &Pairs, id: &str, name: &str, token: &str) {
    *link.next.lock().unwrap() = Some(PairedPhone { id: id.into(), name: name.into(), token: token.into(), ips: vec!["10.0.0.5".into()],
                                                    active_ip: None, cert_sha256: "ab".repeat(32) });
    ui(r, "card", "add-phone", vec![]);
    to_connection(r, |p, host| p.on_event(host, &Event::TimerFired { owner: "connection".into(), token: "pair_poll".into() }));
    ui(r, "card", "add-closed", vec![]);
}

fn iso(r: &Rig) -> f64 {
    r.w.state.borrow().plugin_config("camera_control").unwrap()["iso"].as_f64().unwrap()
}

/// The ISO slider, moved by hand in manual exposure.
fn set_iso(r: &Rig, value: u32) {
    let mut cfg = r.w.state.borrow().plugin_config("camera_control").unwrap();
    cfg["exp_manual"] = json!(true);
    cfg["iso"] = json!(value);
    r.configure("camera_control", cfg);
}

#[test]
fn each_phone_keeps_its_own_settings() {
    let (r, link, _) = window("own-settings");
    pair(&r, &link, "id-a", "A", "tok-a");
    let default_iso = iso(&r);
    set_iso(&r, 400);
    pair(&r, &link, "id-b", "B", "tok-b");
    assert!((iso(&r) - default_iso).abs() <= 1.0, "{}", iso(&r));

    ui(&r, "header", "picked", vec![json!(0)]);
    assert!((iso(&r) - 400.0).abs() <= 1.0, "{}", iso(&r));
}

#[test]
fn phones_and_selection_survive_a_restart() {
    let (r, link, _) = window("restart-before");
    pair(&r, &link, "id-a", "A", "tok-a");
    pair(&r, &link, "id-b", "B", "tok-b");
    ui(&r, "header", "picked", vec![json!(0)]);
    assert!(r.w.state.borrow_mut().save_now());
    let mut saved = r.w.state.borrow().file.load();
    drop(r);

    let (again, _, shared) = window("restart-after");
    again.w.state.borrow().file.save(&mut saved);
    again.w.state.borrow_mut().apply_saved_config();
    assert_eq!(shared.picked_source().as_deref(), Some("id-a"));
    assert_eq!(shared.phones().iter().find(|p| p.id == "id-b").unwrap().token, "tok-b");

    // The panels hold id-a's settings from the start: what changes first is filed under it when the pick moves
    set_iso(&again, 800);
    ui(&again, "header", "picked", vec![json!(1)]);
    let filed = again.w.state.borrow().file.load()["devices"]["id-a"]["plugin_configs"]["camera_control"]["iso"].as_f64();
    assert!(filed.is_some_and(|iso| (iso - 800.0).abs() <= 1.0), "{filed:?}");
}

#[test]
fn removing_a_phone_deletes_its_stored_settings() {
    let (r, link, _) = window("removed");
    pair(&r, &link, "id-a", "A", "tok-a");
    pair(&r, &link, "id-b", "B", "tok-b");
    assert!(r.w.state.borrow_mut().save_now());
    assert!(r.w.state.borrow().file.load()["devices"].get("id-b").is_some());
    ui(&r, "header", "remove", vec![json!("id-b")]);
    assert!(r.w.state.borrow().file.load()["devices"].get("id-b").is_none());
}
