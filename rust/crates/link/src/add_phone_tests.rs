//! tests/test_connection_dialogs.py: Add device with the real pairing server and the real adb commands, only the adb program itself
//! stood in for. What's in plugin.rs's tests drives the card through a stand-in offer.

use super::*;
use crate::adb::{Adb, Outcome};
use crate::connection::Tunnels;
use crate::net::PairingAddress;
use crate::pairing::pairing_proof;
use crate::service::{usb_devices_with, Offer};
use crate::shared::Shared;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;
use std::time::Duration;
use telescope_core::link::{PairingOffer, PhoneSession, RoutePrefs, Unreachable};
use telescope_core::testing::FakeHost;

struct NoTunnels;

impl Tunnels for NoTunnels {
    fn acquire(&self, _serial: &str, _remote_port: u16) -> Option<u16> {
        None
    }
    fn release(&self, _serial: &str, _remote_port: u16) {}
}

/// What the adb program answers, and every command it was given.
#[derive(Default)]
struct FakeAdb {
    /// The lines of `adb devices -l` after its heading.
    devices: Mutex<Vec<String>>,
    /// What `getprop ro.product.model` says, by serial.
    models: Mutex<HashMap<String, String>>,
    calls: Mutex<Vec<Vec<String>>>,
    /// The serials a shell line with `am broadcast` was sent to (on stdin, where the offer must stay), in order.
    sent: Mutex<Vec<String>>,
}

impl FakeAdb {
    fn adb(self: &Arc<FakeAdb>) -> Adb {
        let me = self.clone();
        Adb { exe: Some("adb".into()), runner: Box::new(move |cmd, _, input| me.answer(cmd, input)) }
    }

    fn answer(&self, cmd: &[String], input: Option<&[u8]>) -> Outcome {
        self.calls.lock().unwrap().push(cmd.to_vec());
        if let Some(input) = input.filter(|i| String::from_utf8_lossy(i).contains("am broadcast")) {
            assert!(!cmd.iter().any(|a| a.contains("payload")), "the offer is in adb's arguments: {cmd:?}");
            assert!(String::from_utf8_lossy(input).contains("payload"));
            self.sent.lock().unwrap().push(cmd[2].clone());
        }
        if cmd.iter().any(|c| c == "devices") {
            return (0, format!("List of devices attached\n{}\n", self.devices.lock().unwrap().join("\n")), String::new());
        }
        if cmd.iter().any(|c| c == "getprop") {
            return (0, self.models.lock().unwrap().get(&cmd[2]).cloned().unwrap_or_default(), String::new());
        }
        (0, String::new(), String::new())
    }

    /// The serials `am broadcast` went to, in order.
    fn broadcasts(&self) -> Vec<String> {
        self.sent.lock().unwrap().clone()
    }

    /// How many times this command went to this phone, as (adb -s serial ) + words.
    fn count(&self, words: &[String]) -> usize {
        self.calls.lock().unwrap().iter().filter(|c| c.as_slice() == words).count()
    }
}

struct OfferLink {
    adb: Arc<FakeAdb>,
    models: Mutex<HashMap<String, String>>,
    offer: Mutex<Option<Arc<Offer>>>,
}

impl Link for OfferLink {
    fn new_identity(&self) -> (String, String) {
        ("computer-id".into(), "Desk".into())
    }
    fn resolve(&self, _phone: &PairedPhone, _prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
        Err(Unreachable::NoAnswer)
    }
    fn pair(&self, cid: &str, name: &str) -> Result<Arc<dyn PairingOffer>, String> {
        let adb = self.adb.clone();
        let lan = vec![PairingAddress { ip: "192.168.1.2".into(), interface: "wlan0".into(), kind: "lan" }];
        let offer = Arc::new(Offer::start_on(cid, name, Some(lan), Arc::new(move || adb.adb()))?);
        *self.offer.lock().unwrap() = Some(offer.clone());
        Ok(offer)
    }
    fn adb_available(&self) -> bool {
        true
    }
    fn usb_devices(&self) -> Vec<UsbDevice> {
        usb_devices_with(&self.adb.adb(), &self.models)
    }
}

struct Rig {
    plugin: ConnectionPlugin,
    host: FakeHost,
    adb: Arc<FakeAdb>,
    link: Arc<OfferLink>,
    shared: Arc<Shared>,
}

impl Rig {
    /// Add device open, with these lines in `adb devices -l` and the first look at them taken.
    fn open(devices: &[&str]) -> Rig {
        let adb = Arc::new(FakeAdb::default());
        *adb.devices.lock().unwrap() = devices.iter().map(|d| d.to_string()).collect();
        let link = Arc::new(OfferLink { adb: adb.clone(), models: Mutex::default(), offer: Mutex::default() });
        let shared = Shared::new(Arc::new(NoTunnels));
        let mut r = Rig { plugin: ConnectionPlugin::new(link.clone(), shared.clone()), host: FakeHost::new(), adb, link, shared };
        r.plugin.setup(&mut r.host);
        r.shared.conn().computer_id = "computer-id".into();
        r
    }

    fn add_phone(&mut self) {
        self.ui("add-phone", vec![]);
        self.usb_state();
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
    }

    /// Hands the plugin what its threads posted until a look at the phones has come back.
    fn usb_state(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let mut seen = false;
            for (token, data) in self.host.take_posted() {
                seen |= token == P_USB_STATE;
                self.plugin.on_event(&mut self.host, &Event::Posted { owner: NAME.into(), token, data });
            }
            if seen {
                return;
            }
            assert!(Instant::now() < deadline, "no answer from adb");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    /// _poll_usb: the timer's look at what's plugged in.
    fn poll(&mut self) {
        self.plugin.on_event(&mut self.host, &Event::TimerFired { owner: NAME.into(), token: T_PAIR_USB.into() });
        self.usb_state();
    }

    /// Waits for the broadcasts the plugin's threads are sending, then gives any extra one time to show.
    fn broadcasts(&self, want: usize) -> Vec<String> {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.adb.broadcasts().len() < want && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        std::thread::sleep(Duration::from_millis(40));
        self.adb.broadcasts()
    }

    fn prop(&self, key: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
    }

    fn text(&self, key: &str) -> String {
        self.prop(key).as_str().unwrap_or_default().to_string()
    }

    fn picks(&self) -> Vec<String> {
        self.prop("picks").as_array().map_or_else(Vec::new, |p| p.iter().map(|b| b["label"].as_str().unwrap().to_string()).collect())
    }

    fn offer(&self) -> Arc<Offer> {
        self.link.offer.lock().unwrap().clone().unwrap()
    }

    /// The timer that reads the pairing server, until a phone that paired has come through.
    fn paired(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.shared.phones().is_empty() {
            assert!(Instant::now() < deadline, "the pairing never came through");
            self.plugin.on_event(&mut self.host, &Event::TimerFired { owner: NAME.into(), token: T_PAIR_POLL.into() });
            for (token, data) in self.host.take_posted() {
                self.plugin.on_event(&mut self.host, &Event::Posted { owner: NAME.into(), token, data });
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

/// What a phone that scanned the code sends; the status it gets back, 0 when nothing answers.
fn post_pairing(offer: &Offer, phone_id: &str) -> u16 {
    let payload: Value = serde_json::from_str(&offer.payload()).unwrap();
    let (token, nonce) = (payload["token"].as_str().unwrap(), payload["nonce"].as_str().unwrap());
    let cert = "ab".repeat(32);
    let proof = pairing_proof(token, nonce, phone_id, &cert);
    let body = json!({"name": "Phone", "ips": [], "phone_id": phone_id, "cert_sha256": cert, "proof": proof}).to_string();
    let Ok(mut conn) = std::net::TcpStream::connect(("127.0.0.1", offer.port())) else { return 0 };
    let _ = conn.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = write!(conn, "POST /pair/{nonce} HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}", body.len());
    let mut reply = String::new();
    let _ = conn.read_to_string(&mut reply);
    reply.split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0)
}

const PIXEL: &str = "serial-1 device product:p model:Pixel_7 device:d";

fn adb_words(serial: &str, words: &[&str]) -> Vec<String> {
    ["adb", "-s", serial].iter().chain(words).map(|w| w.to_string()).collect()
}

#[test]
fn shows_a_qr_code_when_the_computer_is_on_a_network() {
    let mut r = Rig::open(&[]);
    r.add_phone();
    assert!(r.prop("qr-size").as_u64().unwrap() > 0);
    assert!(r.text("wifi-text").contains("Scan pairing code"), "{}", r.text("wifi-text"));
    assert!(r.offer().payload().contains("computer-id"));
}

#[test]
fn browser_camera_sits_beside_android_and_picking_it_ends_the_pairing() {
    let mut r = Rig::open(&[]);
    r.add_phone();
    assert_eq!(r.text("other-name"), "", "its plugin is off");
    r.ui("add-closed", vec![]);
    let sources = vec![SourceName { id: "browser".into(), name: "Browser camera".into() }];
    r.plugin.on_event(&mut r.host, &Event::StreamSourcesChanged { sources });
    r.add_phone();
    assert_eq!(r.text("other-name"), "Browser camera");
    r.ui("other-picked", vec![]);
    assert_eq!(r.prop("add-open"), json!(false));
    assert_eq!(r.shared.picked_source().as_deref(), Some("browser"));
}

#[test]
fn an_unauthorized_phone_asks_for_the_debugging_prompt() {
    let mut r = Rig::open(&["serial-1 unauthorized"]);
    r.add_phone();
    assert!(r.text("usb-text").contains("Allow USB debugging"), "{}", r.text("usb-text"));
    assert_eq!(r.broadcasts(0), Vec::<String>::new());
}

#[test]
fn a_plugged_in_phone_waits_to_be_picked() {
    // Left plugged in, it would otherwise pair before the phone being scanned
    let mut r = Rig::open(&[PIXEL]);
    r.add_phone();
    assert!(r.text("usb-text").contains("Pick a phone below"), "{}", r.text("usb-text"));
    assert_eq!(r.picks(), ["Pair Pixel 7"]);
    assert_eq!(r.broadcasts(0), Vec::<String>::new());
}

#[test]
fn buttons_name_the_phone_as_it_names_itself() {
    // adb devices -l turns SM-G975F into SM_G975F
    let mut r = Rig::open(&["serial-1 device model:SM_G975F", "serial-2 device model:Pixel_7"]);
    r.adb.models.lock().unwrap().insert("serial-1".into(), "SM-G975F".into());
    r.add_phone();
    assert_eq!(r.picks(), ["Pair SM-G975F", "Pair Pixel 7"]);
}

#[test]
fn a_picked_phone_gets_the_pairing_offer_over_usb() {
    let mut r = Rig::open(&[PIXEL]);
    r.add_phone();
    let port = r.offer().port();
    r.ui("pick", vec![json!("serial-1")]);
    assert_eq!(r.broadcasts(1), ["serial-1"]);
    let tcp = format!("tcp:{port}");
    let reverse = adb_words("serial-1", &["reverse", &tcp, &tcp]);
    assert_eq!(r.adb.count(&reverse), 1);
    assert!(r.text("usb-text").contains("Pairing Pixel 7"), "{}", r.text("usb-text"));
    assert_eq!(r.prop("picks")[0]["enabled"], false);
    r.poll(); // too soon to send again
    assert_eq!(r.broadcasts(1), ["serial-1"]);
    let long_ago = Instant::now() - Duration::from_secs_f64(OFFER_AGAIN_S + 1.0);
    r.plugin.add.as_mut().unwrap().last_offer.insert("serial-1".into(), long_ago);
    r.poll(); // sent again, in case the app wasn't on screen the first time
    assert_eq!(r.broadcasts(2), ["serial-1", "serial-1"]);
    assert_eq!(r.adb.count(&reverse), 1);
    r.ui("add-closed", vec![]);
    let unreverse = adb_words("serial-1", &["reverse", "--remove", &tcp]);
    let deadline = Instant::now() + Duration::from_secs(5);
    while r.adb.count(&unreverse) == 0 {
        assert!(Instant::now() < deadline, "the reverse stayed");
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn with_several_phones_plugged_in_only_the_picked_one_pairs() {
    let mut r = Rig::open(&[PIXEL, "serial-2 device", "serial-3 unauthorized"]);
    r.add_phone();
    assert_eq!(r.picks(), ["Pair Pixel 7", "Pair serial-2"]);
    r.ui("pick", vec![json!("serial-2")]);
    r.poll();
    assert_eq!(r.broadcasts(1), ["serial-2"]);
}

#[test]
fn unplugging_the_picked_phone_forgets_the_pick() {
    let mut r = Rig::open(&[PIXEL]);
    r.add_phone();
    r.ui("pick", vec![json!("serial-1")]);
    assert_eq!(r.broadcasts(1), ["serial-1"]);
    r.adb.devices.lock().unwrap().clear();
    r.poll();
    assert!(r.picks().is_empty());
    *r.adb.devices.lock().unwrap() = vec![PIXEL.to_string()];
    r.poll();
    assert_eq!(r.prop("picks")[0]["enabled"], true);
    assert_eq!(r.broadcasts(1), ["serial-1"], "only a new pick sends it again");
}

#[test]
fn a_scanned_phone_pairs_while_another_is_plugged_in() {
    let mut r = Rig::open(&[PIXEL]);
    r.add_phone();
    assert_eq!(post_pairing(&r.offer(), "phone-wifi"), 200);
    r.paired();
    assert_eq!(r.shared.phones().iter().map(|p| p.id.as_str()).collect::<Vec<_>>(), ["phone-wifi"]);
    assert_eq!(r.broadcasts(0), Vec::<String>::new());
    assert!(r.picks().is_empty());
}

#[test]
fn success_shows_the_phone_and_hands_the_result_over() {
    let mut r = Rig::open(&[]);
    r.add_phone();
    let offer = r.offer();
    assert_eq!(post_pairing(&offer, "phone-1"), 200);
    r.paired();
    assert_eq!(r.text("result-text"), "Paired with Phone");
    assert_eq!(r.prop("paired"), true);
    assert!(r.plugin.add.as_ref().unwrap().offer.is_none(), "the offer is over");
    assert_ne!(post_pairing(&offer, "phone-2"), 200, "a late duplicate is ignored");
    r.plugin.on_event(&mut r.host, &Event::TimerFired { owner: NAME.into(), token: T_PAIR_POLL.into() });
    assert_eq!(r.shared.phones().len(), 1);
}

#[test]
fn closing_stops_the_pairing_server() {
    let mut r = Rig::open(&[]);
    r.add_phone();
    let held = r.offer();
    r.ui("add-closed", vec![]);
    assert!(r.plugin.add.is_none());
    assert_ne!(post_pairing(&held, "phone-1"), 200);
}
