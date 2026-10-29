//! The link crate behind core's Link, PhoneSession and PairingOffer traits.

use crate::adb::{self, Adb};
use crate::connection::{self, opening_of, with_session, RealPace, SessionCtl, VIDEO_H264};
use crate::discovery::LanDiscovery;
use crate::https::PhoneAuth;
use crate::pairing::{PairingResult, PairingServer};
use crate::phone_client::ControlClient;
use crate::phones::{auth_of, RouteResolver, UsbTunnels, STREAM_PORT};
use crate::sensors::SensorHub;
use crate::shared::Shared;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::sync::{mpsc, Arc, Mutex, OnceLock};
use std::time::Duration;
use telescope_core::link::{Link, PairingOffer, PhoneControl, PhoneSensors, PhoneSession, PhoneStatus, Resolution, RoutePreference,
                           RoutePrefs, StreamFormat, StreamRequest, StreamTarget, Unreachable, UsbDevice};
use telescope_core::media::{Codec, VideoFeed};
use telescope_core::source::{ControlClient as Control, PairedPhone, RouteSpec};

/// How long opening a stream's video waits.
const VIDEO_TIMEOUT: Duration = Duration::from_secs(3);
const INSTALL_TIMEOUT: Duration = Duration::from_secs(120);

/// Finds phones on the LAN from the moment it's made, so a start doesn't wait for mDNS.
pub struct LinkService {
    lan: Arc<LanDiscovery>,
    tunnels: Arc<UsbTunnels>,
    sensors: Arc<SensorHub>,
    /// Phone models by serial, asked once each.
    models: Mutex<HashMap<String, String>>,
    /// The connection model, shared with the Connection plugin.
    shared: Arc<Shared>,
    stills: Arc<crate::stills::Stills>,
}

impl LinkService {
    pub fn new() -> LinkService {
        LinkService::with_discovery(LanDiscovery::start())
    }

    fn with_discovery(lan: LanDiscovery) -> LinkService {
        LinkService::with_tunnels(lan, adb_tunnels(Arc::new(Adb::system)))
    }

    fn with_tunnels(lan: LanDiscovery, tunnels: UsbTunnels) -> LinkService {
        let tunnels = Arc::new(tunnels);
        let shared = Shared::new(tunnels.clone());
        LinkService { lan: Arc::new(lan), tunnels, sensors: Arc::new(SensorHub::new(|_| {})), models: Mutex::default(), shared, stills: Arc::default() }
    }

    /// The model the Connection plugin and the stream flow's route calls share.
    pub fn shared(&self) -> Arc<Shared> {
        self.shared.clone()
    }

    /// The adb forwards streams and checks hold, shared with the Connection model.
    pub fn tunnels(&self) -> Arc<UsbTunnels> {
        self.tunnels.clone()
    }

    pub fn resolver(&self) -> RouteResolver {
        let lan = self.lan.clone();
        RouteResolver {
            adb_available: Box::new(|| Adb::system().available()),
            adb_device_states: Box::new(|| Adb::system().device_states()),
            tunnels: self.tunnels.clone(),
            discover: Box::new(move |id| lan.lookup(id)),
            client: RouteResolver::real_client(),
            wifi_timeout: Duration::from_millis(1500),
        }
    }

    /// Opens a session on route with this video path; controls is a running stream's, carried over.
    pub fn session(&self, phone: &PairedPhone, route: &RouteSpec, video_path: &str, controls: Option<Arc<dyn PhoneControl>>)
                   -> Result<Session, String> {
        let (host, forward) = match route {
            // The Connection model's hold is the stream's only one, so recovery can let it go and open a fresh forward
            RouteSpec::Usb { serial } => match self.tunnels.held_port(serial, STREAM_PORT) {
                Some(local) => (format!("127.0.0.1:{local}"), None),
                None => {
                    let local = self.tunnels.acquire(serial, STREAM_PORT)
                        .ok_or_else(|| telescope_core::i18n::tr("adb couldn't open a connection to the phone. Unplug it, plug it back in and try again."))?;
                    (format!("127.0.0.1:{local}"), Some(serial.clone()))
                }
            },
            RouteSpec::Network { address } => (format!("{address}:{STREAM_PORT}"), None),
        };
        let video_url = format!("https://{host}{video_path}");
        let base = format!("https://{host}/v1");
        let auth = auth_of(phone);
        // Carried controls keep going to the running route; the stream flow retargets them once this one proved itself
        let carried = OnceLock::new();
        if let Some(c) = controls {
            let _ = carried.set(c);
        }
        Ok(Session { phone: phone.clone(), route: route.clone(), video_url, base, auth, forward, controls: carried, tunnels: self.tunnels.clone() })
    }
}

/// Forwards through adb. A forward goes on a thread: the last release often comes with the connection model locked,
/// or on the UI thread, and the bookkeeping is done by then so a new acquire opens a fresh one.
fn adb_tunnels(adb: Arc<dyn Fn() -> Adb + Send + Sync>) -> UsbTunnels {
    let unforward = adb.clone();
    UsbTunnels::new(move |serial, remote| adb().forward_auto(remote, Some(serial)), move |serial, local| {
        let (adb, serial) = (unforward.clone(), serial.to_string());
        std::thread::spawn(move || adb().unforward(local, Some(&serial)));
    })
}

impl Default for LinkService {
    fn default() -> Self {
        LinkService::new()
    }
}

fn unreachable_of(res: &Resolution) -> Unreachable {
    match res.status {
        PhoneStatus::NotPaired => Unreachable::NotPaired,
        PhoneStatus::LocalOnly => Unreachable::LocalOnly,
        PhoneStatus::PhoneOutdated => Unreachable::Outdated("phone app".into()),
        PhoneStatus::DesktopOutdated => Unreachable::Outdated("desktop".into()),
        _ => Unreachable::NoAnswer,
    }
}

impl Link for LinkService {
    fn new_identity(&self) -> (String, String) {
        (crate::util::random_hex(16), connection::default_computer_name())
    }

    fn resolve(&self, phone: &PairedPhone, prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
        let preference = match (prefs.usb, prefs.network) {
            (true, false) => RoutePreference::Usb,
            (false, true) => RoutePreference::Wifi,
            _ => RoutePreference::Auto,
        };
        let res = self.check(phone, preference);
        match (&res.status, &res.route) {
            (PhoneStatus::Ready, Some(route)) => self.open_route(phone, route, VIDEO_H264, None).map_err(|_| Unreachable::NoAnswer),
            _ => Err(unreachable_of(&res)),
        }
    }

    fn pair(&self, computer_id: &str, computer_name: &str) -> Result<Arc<dyn PairingOffer>, String> {
        let (tx, rx) = mpsc::channel::<PairingResult>();
        let tx = Mutex::new(tx);
        let server = PairingServer::start(computer_id, computer_name, None, move |r| {
            let _ = tx.lock().unwrap().send(r);
        }).map_err(|e| e.to_string())?;
        Ok(Arc::new(Offer { server, paired: Mutex::new(rx), reversed: Mutex::default() }))
    }

    fn check(&self, phone: &PairedPhone, preference: RoutePreference) -> Resolution {
        self.resolver().resolve(phone, preference)
    }

    fn open_route(&self, phone: &PairedPhone, route: &RouteSpec, video_path: &str, controls: Option<Arc<dyn PhoneControl>>)
                  -> Result<Box<dyn PhoneSession>, String> {
        Ok(Box::new(self.session(phone, route, video_path, controls)?))
    }

    fn ensure_streaming(&self, phone: &PairedPhone, route: &RouteSpec, opening: Option<StreamRequest>, after_stop: bool,
                        progress: &dyn Fn(&str)) -> Result<(), String> {
        let opening = opening.map(|r| opening_of(r.width, r.height, r.fps));
        let auth = auth_of(phone);
        with_session(Some(&auth), Some(route), &*self.tunnels, |client| {
            connection::ensure_streaming(client.map(|c| c as &dyn SessionCtl), opening.as_ref(), after_stop, progress, &RealPace)
        })
    }

    fn stop_streaming(&self, phone: &PairedPhone, route: &RouteSpec) {
        stop_over(&*self.tunnels, phone, route);
    }

    fn unpair(&self, phone: &PairedPhone) {
        // Unpair on the phone too when it's reachable, so it stops accepting a forgotten computer
        let res = self.check(phone, RoutePreference::Auto);
        let auth = auth_of(phone);
        with_session(Some(&auth), res.route.as_ref(), &*self.tunnels, |client| {
            if let Some(c) = client {
                c.unpair();
            }
        });
    }

    fn adb_available(&self) -> bool {
        Adb::system().available()
    }

    fn usb_devices(&self) -> Vec<UsbDevice> {
        let adb = Adb::system();
        let mut models = self.models.lock().unwrap();
        adb.device_models().into_iter().map(|(serial, state, model)| {
            if state == "device" && !models.contains_key(&serial) {
                let asked = adb.model(&serial);
                models.insert(serial.clone(), if asked.is_empty() { model.clone() } else { asked });
            }
            let model = models.get(&serial).cloned().unwrap_or(model);
            UsbDevice { serial, state, model }
        }).collect()
    }

    fn bundled_apk(&self) -> Option<String> {
        adb::bundled_apk_path().map(|p| p.to_string_lossy().into_owned())
    }

    fn install_app(&self, serial: &str, apk: &str) -> Result<(), String> {
        Adb::system().install(serial, apk, INSTALL_TIMEOUT)
    }

    fn sensors(&self) -> Option<Arc<dyn PhoneSensors>> {
        Some(self.sensors.clone())
    }

    fn start_route(&self, source_id: Option<&str>, format: StreamFormat) -> Result<StreamTarget, String> {
        self.shared.start_route(source_id, format, &|phone, pref| self.check(phone, pref))
    }

    fn session_target(&self, source_id: Option<&str>) -> Option<(PairedPhone, RouteSpec)> {
        self.shared.session_target(source_id)
    }

    fn release_stream(&self, source_id: &str) {
        self.shared.release_stream(source_id);
    }

    fn prepare_route(&self, source_id: &str, route: &RouteSpec) -> Option<String> {
        self.shared.prepare_route(source_id, route)
    }

    fn commit_route(&self, source_id: &str) {
        self.shared.commit_route(source_id);
    }

    fn drop_route(&self, source_id: &str, failed: bool) {
        self.shared.drop_route(source_id, failed);
    }

    fn recovery_check(&self, source_id: Option<&str>) -> Resolution {
        self.shared.recovery_check(source_id, &|phone, pref| self.check(phone, pref))
    }

    fn adopt_route(&self, source_id: Option<&str>, route: &RouteSpec) -> Option<String> {
        self.shared.adopt_route(source_id, route)
    }

    fn show_problem(&self, source_id: Option<&str>, res: &Resolution) {
        self.shared.show_problem(source_id, res);
    }

    fn phones(&self) -> Vec<PairedPhone> {
        self.shared.phones()
    }

    fn picked_source(&self) -> Option<String> {
        self.shared.picked_source()
    }

    fn stills(&self) -> Option<Arc<dyn telescope_core::link::PhoneStills>> {
        Some(self.stills.clone())
    }

    fn shutdown(&self) {
        self.stills.shutdown();
        self.lan.stop();
        self.sensors.shutdown();
        adb::stop_adb_server();
    }
}

/// One phone on one route: its video, its camera controls, and the forward it holds over USB.
pub struct Session {
    pub phone: PairedPhone,
    pub route: RouteSpec,
    pub video_url: String,
    pub base: String,
    auth: PhoneAuth,
    forward: Option<String>,
    /// Made when first asked for, so a start whose wake fails leaves no control thread behind (Python makes the
    /// client after the wake).
    controls: OnceLock<Arc<dyn PhoneControl>>,
    tunnels: Arc<UsbTunnels>,
}

impl Session {
    /// Whether its controls exist yet (and with them their worker thread).
    fn has_controls(&self) -> bool {
        self.controls.get().is_some()
    }

    fn the_controls(&self) -> &Arc<dyn PhoneControl> {
        self.controls.get_or_init(|| Arc::new(ControlClient::new(&self.video_url, self.auth.clone())))
    }
}

impl Control for Session {
    fn send(&self, body: &Value) -> Result<(), String> {
        self.the_controls().send(body);
        Ok(())
    }

    fn state(&self) -> Result<Value, String> {
        self.the_controls().get_state().ok_or_else(|| "the phone didn't answer with its state".into())
    }

    fn has_audio(&self) -> bool {
        true
    }
}

impl PhoneSession for Session {
    fn route(&self) -> RouteSpec {
        self.route.clone()
    }

    fn describe(&self) -> String {
        match &self.route {
            RouteSpec::Usb { serial } => format!("USB ({serial})"),
            RouteSpec::Network { address } => format!("Wi-Fi ({address})"),
        }
    }

    fn start(&self, r: StreamRequest) -> Result<(), String> {
        let opening = opening_of(r.width, r.height, r.fps);
        with_session(Some(&self.auth), Some(&self.route), &*self.tunnels, |client| {
            connection::ensure_streaming(client.map(|c| c as &dyn SessionCtl), Some(&opening), false, &|_| {}, &RealPace)
        })
    }

    fn stop(&self) -> Result<(), String> {
        with_session(Some(&self.auth), Some(&self.route), &*self.tunnels, |client| match client.map(|c| c.stop()) {
            Some(r) if r.ok => Ok(()),
            Some(r) => Err(r.error.unwrap_or_default()),
            None => Err(telescope_core::i18n::tr("Lost the connection to the phone. Try again.")),
        })
    }

    fn open_video(&self) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        crate::feeds::open_feed(&self.video_url, &self.auth, VIDEO_TIMEOUT)
    }

    fn release(&self) {
        if let Some(serial) = &self.forward {
            self.tunnels.release(serial, STREAM_PORT);
        }
    }

    fn video_url(&self) -> String {
        self.video_url.clone()
    }

    fn stream_base(&self) -> String {
        self.base.clone()
    }

    fn open_audio(&self) -> Result<Box<dyn telescope_core::audio::AudioFeed>, String> {
        Ok(Box::new(crate::audio::open_audio(&format!("{}/audio", self.base), &self.auth)?))
    }

    fn controls(&self) -> Option<Arc<dyn PhoneControl>> {
        Some(self.the_controls().clone())
    }
}

/// The pairing server's offer, and the phone it hands back once one pairs.
struct Offer {
    server: PairingServer,
    paired: Mutex<mpsc::Receiver<PairingResult>>,
    /// Serials with an adb reverse to the pairing server, taken down when the offer goes.
    reversed: Mutex<HashSet<String>>,
}

impl Offer {
    /// Sends the USB offer to one phone (again: the broadcast only lands while the app is on screen).
    fn offer_to(&self, adb: &Adb, serial: &str, package: &str) -> bool {
        let b64 = crate::util::base64(self.server.offer.usb_payload.as_bytes());
        if self.reversed.lock().unwrap().insert(serial.to_string()) {
            adb.reverse(self.server.offer.port, Some(serial));
        }
        adb.broadcast_pair(&b64, Some(serial), package).0
    }
}

impl PairingOffer for Offer {
    fn payload(&self) -> String {
        self.server.offer.payload.clone()
    }

    fn port(&self) -> u16 {
        self.server.offer.port
    }

    fn addresses(&self) -> Vec<String> {
        self.server.offer.candidates.iter().map(|c| c.ip.clone()).collect()
    }

    fn offer_over_usb(&self, package: &str) -> usize {
        let adb = Adb::system();
        adb.devices().iter().filter(|serial| self.offer_to(&adb, serial, package)).count()
    }

    fn poll(&self) -> Option<PairedPhone> {
        self.paired.lock().unwrap().try_recv().ok().map(|r| r.phone())
    }

    fn offer_to(&self, serial: &str, package: &str) -> bool {
        Offer::offer_to(self, &Adb::system(), serial, package)
    }
}

impl Drop for Offer {
    fn drop(&mut self) {
        self.server.stop();
        let port = self.server.offer.port;
        let serials: Vec<String> = self.reversed.lock().unwrap().drain().collect();
        if !serials.is_empty() {
            std::thread::spawn(move || {
                let adb = Adb::system();
                for serial in serials {
                    adb.unreverse(port, Some(&serial));
                }
            });
        }
    }
}

/// Asks the phone to stop its camera over route (stop_phone_streaming).
fn stop_over(tunnels: &dyn connection::Tunnels, phone: &PairedPhone, route: &RouteSpec) {
    let auth = auth_of(phone);
    with_session(Some(&auth), Some(route), tunnels, |client| {
        if let Some(c) = client {
            c.stop();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;

    #[test]
    fn a_phone_pairs_through_the_offer() {
        let link = LinkService::with_discovery(LanDiscovery::default());
        let offer = link.pair("computer", "desk").unwrap();
        let payload: Value = serde_json::from_str(&offer.payload()).unwrap();
        let (nonce, token) = (payload["nonce"].as_str().unwrap(), payload["token"].as_str().unwrap());
        let cert = "ab".repeat(32);
        let proof = crate::util::pairing_proof(token, nonce, "phone-1", &cert);
        let body = serde_json::json!({"name": "Pixel 8", "phone_id": "phone-1", "cert_sha256": cert, "proof": proof, "ips": ["192.168.1.21"]}).to_string();
        let mut conn = TcpStream::connect(("127.0.0.1", offer.port())).unwrap();
        write!(conn, "POST /pair/{nonce} HTTP/1.1\r\nContent-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        let mut reply = String::new();
        conn.read_to_string(&mut reply).unwrap();
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let phone = loop {
            if let Some(p) = offer.poll() {
                break p;
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!((phone.name.as_str(), phone.token.as_str(), phone.active_ip), ("Pixel 8", token, None));
    }

    #[test]
    fn a_wifi_session_reaches_the_stream_port_at_the_phones_address() {
        let link = LinkService::with_discovery(LanDiscovery::default());
        let phone = PairedPhone { id: "p".into(), name: "P".into(), token: "t".into(), ips: vec![], active_ip: None, cert_sha256: "ab".repeat(32) };
        let session = link.session(&phone, &RouteSpec::Network { address: "10.0.0.5".into() }, "/v1/video.udp", None).unwrap();
        assert_eq!((session.video_url.as_str(), session.base.as_str()), ("https://10.0.0.5:8080/v1/video.udp", "https://10.0.0.5:8080/v1"));
        assert_eq!(session.describe(), "Wi-Fi (10.0.0.5)");
        session.controls().unwrap().close();
    }

    #[test]
    fn a_hand_over_takes_the_controls_along() {
        let link = LinkService::with_discovery(LanDiscovery::default());
        let phone = PairedPhone { id: "p".into(), name: "P".into(), token: "t".into(), ips: vec![], active_ip: None, cert_sha256: "ab".repeat(32) };
        let first = link.session(&phone, &RouteSpec::Network { address: "10.0.0.5".into() }, VIDEO_H264, None).unwrap();
        let client = Arc::new(ControlClient::new(&first.video_url, first.auth.clone()));
        let controls: Arc<dyn PhoneControl> = client.clone();
        let second = link.session(&phone, &RouteSpec::Network { address: "10.0.0.6".into() }, VIDEO_H264, Some(controls.clone())).unwrap();
        assert!(Arc::ptr_eq(&second.controls().unwrap(), &controls));
        // They keep going to the running stream until the new route proved itself (the stream flow retargets them)
        assert_eq!(client.base(), first.base);
        first.controls().unwrap().close();
        second.controls().unwrap().close();
    }

    #[test]
    fn recovery_over_the_same_cable_opens_a_fresh_forward() {
        use std::sync::atomic::{AtomicU16, Ordering};
        let forwards = Arc::new(AtomicU16::new(0));
        let counted = forwards.clone();
        let tunnels = UsbTunnels::new(move |_, _| Some(41000 + counted.fetch_add(1, Ordering::SeqCst)), |_, _| {});
        let link = LinkService::with_tunnels(LanDiscovery::default(), tunnels);
        let phone = PairedPhone { id: "p".into(), name: "P".into(), token: "t".into(), ips: vec![], active_ip: None, cert_sha256: "ab".repeat(32) };
        link.shared.conn().on_phone_paired(phone.clone(), |_| false);
        let usb = RouteSpec::Usb { serial: "serial-1".into() };
        let ready = Resolution { route: Some(usb.clone()), ..Resolution::new(PhoneStatus::Ready) };
        let format = crate::connection::StreamFormat { h264: true, dynamic: false };
        let target = link.shared.start_route(Some("p"), format, &|_, _| ready.clone()).unwrap();
        let session = link.session(&phone, &usb, VIDEO_H264, None).unwrap();
        assert_eq!(session.video_url, target.video_url);
        // The cable went and came back: adb dropped the forward, so recovery needs a new one
        let url = link.shared.adopt_route(Some("p"), &usb).unwrap();
        assert_eq!((forwards.load(Ordering::SeqCst), url), (2, format!("https://127.0.0.1:41001{VIDEO_H264}")));
        session.release();
        link.shared.release_stream("p");
        assert_eq!(link.tunnels.held_port("serial-1", STREAM_PORT), None);
        session.controls().unwrap().close();
    }

    #[test]
    fn a_forward_goes_without_waiting_for_adb() {
        let (tx, rx) = mpsc::channel::<Vec<String>>();
        let tx = Mutex::new(tx);
        let tunnels = adb_tunnels(Arc::new(move || {
            let tx = Mutex::new(tx.lock().unwrap().clone());
            Adb { exe: Some("adb".into()), runner: Box::new(move |cmd, _| {
                if cmd.iter().any(|a| a == "--remove") {
                    std::thread::sleep(Duration::from_millis(800)); // a stalled adb
                }
                let _ = tx.lock().unwrap().send(cmd.to_vec());
                (0, "41000\n".into(), String::new())
            }) }
        }));
        assert_eq!(tunnels.acquire("serial-1", STREAM_PORT), Some(41000));
        let started = std::time::Instant::now();
        tunnels.release("serial-1", STREAM_PORT);
        assert!(started.elapsed() < Duration::from_millis(300), "release waited {:?} for adb", started.elapsed());
        let removed = rx.iter().find(|cmd| cmd.iter().any(|a| a == "--remove"));
        assert!(removed.is_some_and(|cmd| cmd.contains(&"tcp:41000".to_string())));
    }

    #[test]
    fn a_session_starts_its_controls_only_once_asked_for_them() {
        let link = LinkService::with_discovery(LanDiscovery::default());
        let phone = PairedPhone { id: "p".into(), name: "P".into(), token: "t".into(), ips: vec![], active_ip: None, cert_sha256: "ab".repeat(32) };
        // A start whose wake failed: the session goes without a control thread left behind
        let failed = link.session(&phone, &RouteSpec::Network { address: "10.0.0.5".into() }, VIDEO_H264, None).unwrap();
        assert!(!failed.has_controls());
        failed.release();
        let woke = link.session(&phone, &RouteSpec::Network { address: "10.0.0.5".into() }, VIDEO_H264, None).unwrap();
        let controls = woke.controls().unwrap();
        assert!(woke.has_controls() && Arc::ptr_eq(&controls, &woke.controls().unwrap()));
        controls.close();
    }

    struct ToServer(u16);

    impl connection::Tunnels for ToServer {
        fn acquire(&self, _serial: &str, _remote_port: u16) -> Option<u16> {
            Some(self.0)
        }

        fn release(&self, _serial: &str, _remote_port: u16) {}
    }

    #[test]
    fn stop_phone_streaming_posts_stop() {
        use crate::https::testing::{pin_of, serve};
        let server = serve(Some("phone"), Arc::new(|_req| (200, "application/json".into(), br#"{"ok": true}"#.to_vec())));
        let phone = PairedPhone { id: "p".into(), name: "P".into(), token: "tok".into(), ips: vec![], active_ip: None, cert_sha256: pin_of("phone") };
        stop_over(&ToServer(server.port), &phone, &RouteSpec::Usb { serial: "serial-1".into() });
        let seen = server.seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 1);
        assert_eq!((seen[0].method.as_str(), seen[0].path.as_str()), ("POST", "/v1/session"));
        assert_eq!(serde_json::from_slice::<Value>(&seen[0].body).unwrap(), serde_json::json!({"action": "stop"}));
    }
}
