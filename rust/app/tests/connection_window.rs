// Per-phone settings through the real window with the real Connection and Camera plugins.
mod rig;

use rig::Rig;
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};
use telescope_core::event::Event;
use telescope_core::link::{Link, PairingOffer, PhoneSession, PhoneStatus, Resolution, RoutePreference, RoutePrefs, Unreachable};
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
    /// Every check finds the phone saying it doesn't know this computer any more.
    dropped: Mutex<bool>,
}

struct Offer(Option<PairedPhone>, Mutex<bool>);

impl PairingOffer for Offer {
    fn payload(&self) -> String {
        "{}".into()
    }
    fn port(&self) -> u16 {
        7479
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
    fn check(&self, _phone: &PairedPhone, _preference: RoutePreference) -> Resolution {
        Resolution::new(if *self.dropped.lock().unwrap() { PhoneStatus::NotPaired } else { PhoneStatus::Unreachable })
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

/// The settings file as a restart sees it: loaded into a fresh window, which cleans it up the way a start does.
fn restarted(r: Rig, name: &str) -> (Rig, Arc<Shared>) {
    let mut saved = r.w.state.borrow().file.load();
    drop(r);
    let (again, _, shared) = window(name);
    again.w.state.borrow().file.save(&mut saved);
    again.w.state.borrow_mut().apply_saved_config();
    (again, shared)
}

fn listed(r: &Rig) -> Vec<String> {
    r.w.state.borrow().file.load()["plugin_configs"]["connection"]["phones"].as_array().unwrap().iter()
        .map(|p| p["id"].as_str().unwrap().to_string()).collect()
}

#[test]
fn removing_a_phone_leaves_the_other_phones_browsers_and_webcams_alone() {
    let (r, link, _) = window("removed-others");
    pair(&r, &link, "id-a", "A", "tok-a");
    pair(&r, &link, "id-b", "B", "tok-b");
    set_iso(&r, 400);
    assert!(r.w.state.borrow_mut().save_now());
    let mut cfg = r.w.state.borrow().file.load();
    let entry = cfg["devices"]["id-b"].clone();
    for id in ["browser:abc", "webcam:cam1"] {
        cfg["devices"][id] = entry.clone();
        cfg["output_slots"][id] = json!(1);
    }
    cfg["output_slots"]["id-b"] = json!(2);
    r.w.state.borrow().file.save(&mut cfg);
    ui(&r, "header", "remove", vec![json!("id-b")]);
    let saved = r.w.state.borrow().file.load();
    assert!(saved["devices"].get("id-b").is_none() && saved["output_slots"].get("id-b").is_none());
    assert!(["id-a", "browser:abc", "webcam:cam1"].iter().all(|id| saved["devices"].get(*id).is_some()), "{saved}");
    assert_eq!(saved["output_slots"], json!({"browser:abc": 1, "webcam:cam1": 1}));
    assert_eq!(listed(&r), ["id-a"]);
    let (again, _) = restarted(r, "removed-others-restart");
    let kept = again.w.state.borrow().file.load();
    assert!(["id-a", "browser:abc", "webcam:cam1"].iter().all(|id| kept["devices"].get(*id).is_some()), "{kept}");
}

#[test]
fn a_phone_that_dropped_the_pairing_keeps_its_settings_while_it_is_listed() {
    let (r, link, _) = window("phone-dropped");
    pair(&r, &link, "id-a", "A", "tok-a");
    set_iso(&r, 400);
    assert!(r.w.state.borrow_mut().save_now());
    *link.dropped.lock().unwrap() = true;
    ui(&r, "header", "picked", vec![json!(0)]);
    r.skip(10_000);
    assert_eq!(listed(&r), ["id-a"], "the desktop still lists the phone");
    assert!(r.w.state.borrow_mut().save_now());
    assert!(r.w.state.borrow().file.load()["devices"].get("id-a").is_some());
    let (again, shared) = restarted(r, "phone-dropped-restart");
    assert_eq!(shared.phones().len(), 1);
    assert!(again.w.state.borrow().file.load()["devices"].get("id-a").is_some());
    assert!((iso(&again) - 400.0).abs() <= 1.0, "{}", iso(&again));
}

#[test]
fn pairing_a_listed_phone_again_keeps_its_settings() {
    let (r, link, shared) = window("repaired-same-id");
    pair(&r, &link, "id-a", "A", "tok-a");
    set_iso(&r, 400);
    assert!(r.w.state.borrow_mut().save_now());
    pair(&r, &link, "id-a", "A", "tok-new");
    assert_eq!(shared.phones().iter().map(|p| p.token.clone()).collect::<Vec<_>>(), ["tok-new"]);
    assert!((iso(&r) - 400.0).abs() <= 1.0, "{}", iso(&r));
    assert!(r.w.state.borrow_mut().save_now());
    assert!(r.w.state.borrow().file.load()["devices"].get("id-a").is_some());
    let (again, _) = restarted(r, "repaired-same-id-restart");
    assert!((iso(&again) - 400.0).abs() <= 1.0, "{}", iso(&again));
}

#[test]
fn a_reinstalled_phone_confirmed_as_the_same_one_takes_its_settings_to_the_new_pairing() {
    let (r, link, shared) = window("reinstalled");
    pair(&r, &link, "id-old", "Pixel", "tok-old");
    set_iso(&r, 400);
    assert!(r.w.state.borrow_mut().save_now());
    pair(&r, &link, "id-new", "Pixel", "tok-new"); // asks "Same phone?" once the old id has been checked
    for _ in 0..200 {
        if shared.phones().iter().any(|p| p.id == "id-new") {
            break;
        }
        to_connection(&r, |p, host| p.on_issue_action(host, "same_phone", "same_phone:yes"));
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(shared.phones().iter().map(|p| p.id.clone()).collect::<Vec<_>>(), ["id-new"]);
    assert!((iso(&r) - 400.0).abs() <= 1.0, "{}", iso(&r));
    let saved = r.w.state.borrow().file.load();
    assert_eq!(listed(&r), ["id-new"]);
    assert!(saved["devices"].get("id-new").is_some(), "{saved}");
    assert!(saved["devices"].get("id-old").is_none(), "{saved}");
    let (again, _) = restarted(r, "reinstalled-restart");
    let after = again.w.state.borrow().file.load();
    assert!(after["devices"].get("id-new").is_some());
    assert!((iso(&again) - 400.0).abs() <= 1.0, "{} {after}", iso(&again));
}

#[test]
fn saying_it_is_not_the_same_phone_leaves_the_old_entry_and_its_settings_alone() {
    let (r, link, shared) = window("reinstalled-no");
    pair(&r, &link, "id-old", "Pixel", "tok-old");
    set_iso(&r, 400);
    assert!(r.w.state.borrow_mut().save_now());
    pair(&r, &link, "id-new", "Pixel", "tok-new");
    for _ in 0..200 {
        if shared.phones().iter().any(|p| p.id == "id-new") {
            break;
        }
        to_connection(&r, |p, host| p.on_issue_action(host, "same_phone", "same_phone:no"));
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert_eq!(listed(&r), ["id-old", "id-new"]);
    assert!(r.w.state.borrow_mut().save_now());
    let (again, _) = restarted(r, "reinstalled-no-restart");
    assert!(again.w.state.borrow().file.load()["devices"].get("id-old").is_some());
}

#[test]
fn a_phone_whose_pairing_entry_was_damaged_keeps_its_settings_on_later_starts() {
    let (r, link, _) = window("damaged-entry");
    pair(&r, &link, "id-a", "A", "tok-a");
    pair(&r, &link, "id-b", "B", "tok-b");
    set_iso(&r, 400);
    assert!(r.w.state.borrow_mut().save_now());
    // B's entry loses its id, as a hand edit or a bad disk can leave it; A's stays whole
    let mut cfg = r.w.state.borrow().file.load();
    cfg["plugin_configs"]["connection"]["phones"][1].as_object_mut().unwrap().remove("id");
    r.w.state.borrow().file.save(&mut cfg);
    let (again, _) = restarted(r, "damaged-entry-first");
    // Connection dropped the entry it couldn't read; the next save lists A alone
    assert!(again.w.state.borrow_mut().save_now());
    assert_eq!(listed(&again), ["id-a"]);
    let (third, _) = restarted(again, "damaged-entry-second");
    let kept = third.w.state.borrow().file.load();
    assert!(kept["devices"].get("id-b").is_some(), "{kept}");
    assert!(kept["devices"].get("id-a").is_some(), "{kept}");
}
