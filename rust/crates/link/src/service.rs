//! The link crate behind core's Link, PhoneSession and PairingOffer traits.

use crate::discovery::LanDiscovery;
use crate::https::{self, Response};
use crate::pairing::{self, PairingServer, PAIRING_PORT};
use crate::session::{self, Route, RouteKind};
use serde_json::Value;
use std::io::Read;
use std::net::TcpStream;
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use telescope_core::link::{Link, PairingOffer, PhoneSession, RoutePrefs, StreamRequest, Unreachable};
use telescope_core::media::{Codec, FeedEnd, FeedStopper, Packet, VideoFeed};
use telescope_core::source::{ControlClient, PairedPhone, RouteSpec};

const CHUNK: usize = 64 * 1024;

/// Finds phones on the LAN from the moment it's made, so a start doesn't wait for mDNS.
pub struct LinkService {
    lan: LanDiscovery,
}

impl LinkService {
    pub fn new() -> LinkService {
        LinkService { lan: LanDiscovery::start() }
    }
}

impl Default for LinkService {
    fn default() -> Self {
        LinkService::new()
    }
}

impl Link for LinkService {
    fn new_identity(&self) -> (String, String) {
        (crate::util::random_hex(16), crate::util::hostname())
    }

    fn resolve(&self, phone: &PairedPhone, prefs: RoutePrefs) -> Result<Box<dyn PhoneSession>, Unreachable> {
        let lan = prefs.network.then_some(&self.lan);
        session::resolve(phone, lan, prefs.usb, prefs.network).map(|route| Box::new(Session { route }) as Box<dyn PhoneSession>)
    }

    fn pair(&self, computer_id: &str, computer_name: &str) -> Result<Arc<dyn PairingOffer>, String> {
        let server = PairingServer::start(computer_id, computer_name).map_err(|e| e.to_string())?;
        Ok(Arc::new(Offer { offer: server.offer, paired: Mutex::new(server.paired) }))
    }
}

/// One phone on one route.
pub struct Session {
    pub route: Route,
}

impl ControlClient for Session {
    fn send(&self, body: &Value) -> Result<(), String> {
        self.route.control(body).map_err(|e| e.to_string())
    }

    fn state(&self) -> Result<Value, String> {
        self.route.state().map_err(|e| e.to_string())
    }

    fn has_audio(&self) -> bool {
        false
    }
}

impl PhoneSession for Session {
    fn route(&self) -> RouteSpec {
        match &self.route.kind {
            RouteKind::Usb { serial } => RouteSpec::Usb { serial: serial.clone() },
            RouteKind::Wifi => RouteSpec::Network { address: self.route.auth.host.clone() },
        }
    }

    fn describe(&self) -> String {
        self.route.describe()
    }

    fn start(&self, r: StreamRequest) -> Result<(), String> {
        self.route.start(r.width, r.height, r.fps)
    }

    fn stop(&self) -> Result<(), String> {
        self.route.stop()
    }

    fn open_video(&self) -> Result<(Codec, Box<dyn VideoFeed>), String> {
        let resp = self.route.auth.request(self.route.stream_port, "GET", "/v1/video.h264", None, Duration::from_secs(5))
            .map_err(|e| format!("Couldn't open the video: {e}"))?;
        if resp.status != 200 || !resp.header("content-type").unwrap_or("").contains("video/h264") {
            return Err(format!("The phone answered {} instead of H.264", resp.status));
        }
        let socket = resp.aborter().map_err(|e| e.to_string())?;
        Ok((Codec::H264, Box::new(H264Feed { resp, socket, buf: vec![0; CHUNK] })))
    }

    fn release(&self) {
        self.route.release();
    }
}

/// /v1/video.h264 as it arrives.
struct H264Feed {
    resp: Response,
    socket: TcpStream,
    buf: Vec<u8>,
}

struct Stopper(TcpStream);

impl FeedStopper for Stopper {
    fn stop(&self) {
        https::abort(&self.0);
    }
}

impl VideoFeed for H264Feed {
    fn read(&mut self) -> Result<Packet, FeedEnd> {
        match self.resp.body.read(&mut self.buf) {
            Ok(0) => Err(FeedEnd::Closed),
            Ok(n) => Ok(Packet { data: self.buf[..n].to_vec(), time_ns: telescope_core::time::monotonic_ns() }),
            Err(e) => Err(FeedEnd::Error(e.to_string())),
        }
    }

    fn stopper(&self) -> Box<dyn FeedStopper> {
        Box::new(Stopper(self.socket.try_clone().expect("socket handle")))
    }
}

/// The pairing server's offer, and the phone it hands back once one pairs.
struct Offer {
    offer: pairing::PairingOffer,
    paired: Mutex<Receiver<PairedPhone>>,
}

impl PairingOffer for Offer {
    fn payload(&self) -> String {
        self.offer.payload.clone()
    }

    fn port(&self) -> u16 {
        self.offer.port
    }

    fn addresses(&self) -> Vec<String> {
        self.offer.candidates.iter().map(|c| c.ip.clone()).collect()
    }

    fn offer_over_usb(&self, package: &str) -> usize {
        let b64 = crate::util::base64(self.offer.usb_payload.as_bytes());
        let mut sent = 0;
        for serial in crate::adb::devices() {
            if self.offer.port == PAIRING_PORT {
                crate::adb::reverse(&serial, self.offer.port);
            }
            sent += crate::adb::broadcast_pair(&serial, package, &b64) as usize;
        }
        sent
    }

    fn poll(&self) -> Option<PairedPhone> {
        self.paired.lock().unwrap().try_recv().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_phone_pairs_through_the_offer() {
        let link = LinkService { lan: LanDiscovery::default() };
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
        let phone = offer.poll().unwrap();
        assert_eq!((phone.name.as_str(), phone.token.as_str(), phone.active_ip), ("Pixel 8", token, None));
    }
}
