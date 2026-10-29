//! What LinkService and the Connection plugin share: the connection model the stream flow's route calls change, and
//! a way to tell the plugin about it on the host thread (it shows the card and the Start banner).

use crate::connection::{default_computer_name, Connection, StreamFormat, Tunnels};
use crate::phones::STREAM_PORT;
use crate::util::random_hex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;
use telescope_core::host::Poster;
use telescope_core::link::{PhoneStatus, Resolution, RoutePreference, StreamTarget};
use telescope_core::source::{PairedPhone, RouteSpec};

/// Posted when the model changed under the plugin (a route moved, a check landed); data {"save": bool}.
pub const POST_CHANGED: &str = "changed";
/// Posted when a start (or a dropped stream) can't go on; data is a StartProblem.
pub const POST_START_PROBLEM: &str = "start_problem";

/// Why a stream couldn't start, for the banner the Connection plugin shows.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StartProblem {
    NoPhone,
    /// The check found the phone can't stream now.
    Unreachable { source_id: String, resolution: Resolution },
    /// adb couldn't open the forward to the phone.
    Forward { source_id: String },
}

/// Checks how a phone can be reached (RouteResolver.resolve); a parameter so tests can answer for it.
pub type Check<'a> = &'a dyn Fn(&PairedPhone, RoutePreference) -> Resolution;

pub struct Shared {
    conn: Mutex<Connection>,
    /// The format the stream flow last started with, which the plugin's UDP switch checks against.
    format: Mutex<StreamFormat>,
    poster: Mutex<Option<Arc<dyn Poster>>>,
    started: Instant,
}

impl Shared {
    /// A model with a fresh identity until the plugin's set_config brings the saved one.
    pub fn new(tunnels: Arc<dyn Tunnels>) -> Arc<Shared> {
        Arc::new(Shared {
            conn: Mutex::new(Connection::new(random_hex(16), default_computer_name(), tunnels)),
            format: Mutex::default(),
            poster: Mutex::new(None),
            started: Instant::now(),
        })
    }

    pub fn conn(&self) -> MutexGuard<'_, Connection> {
        self.conn.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Seconds on a monotonic clock, as the model's timings want them.
    pub fn now(&self) -> f64 {
        self.started.elapsed().as_secs_f64()
    }

    pub fn format(&self) -> StreamFormat {
        *self.format.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn set_format(&self, format: StreamFormat) {
        *self.format.lock().unwrap_or_else(|e| e.into_inner()) = format;
    }

    /// The plugin's poster (None once it's gone).
    pub fn set_poster(&self, poster: Option<Arc<dyn Poster>>) {
        *self.poster.lock().unwrap_or_else(|e| e.into_inner()) = poster;
    }

    fn post(&self, token: &str, data: Value) {
        let poster = self.poster.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(poster) = poster {
            poster.post(token, data);
        }
    }

    fn changed(&self, save: bool) {
        self.post(POST_CHANGED, json!({"save": save}));
    }

    fn problem(&self, problem: StartProblem) {
        self.post(POST_START_PROBLEM, serde_json::to_value(problem).unwrap_or(Value::Null));
    }

    /// get_stream_info without the virtual camera: check the phone, hold the route, the video URL.
    pub fn start_route(&self, pid: Option<&str>, format: StreamFormat, check: Check) -> Result<StreamTarget, String> {
        self.set_format(format);
        let (phone, preference) = {
            let c = self.conn();
            let pid = pid.map(str::to_string).or_else(|| c.selected_id.clone());
            (pid.and_then(|p| c.phone(Some(&p)).cloned()), c.route_pref)
        };
        let Some(phone) = phone else {
            self.problem(StartProblem::NoPhone);
            return Err("no phone".into());
        };
        // Off the lock: a check takes seconds, and the card keeps drawing meanwhile
        let res = check(&phone, preference);
        let mut c = self.conn();
        let save = c.took_start_check(Some(&phone.id), res.clone());
        let route = match (&res.status, &res.route) {
            (PhoneStatus::Ready, Some(route)) => route.clone(),
            _ => {
                drop(c);
                self.changed(save);
                let status = crate::connection::status_name(res.status);
                self.problem(StartProblem::Unreachable { source_id: phone.id.clone(), resolution: res });
                return Err(format!("phone {status}"));
            }
        };
        let url = c.hold_stream_route(&route, Some(&phone.id), format);
        drop(c);
        self.changed(save);
        let Some(video_url) = url else {
            self.problem(StartProblem::Forward { source_id: phone.id.clone() });
            return Err("adb couldn't open a forward".into());
        };
        Ok(StreamTarget { phone, route, video_url })
    }

    pub fn session_target(&self, pid: Option<&str>) -> Option<(PairedPhone, RouteSpec)> {
        match self.conn().session_target(pid) {
            (Some(phone), Some(route)) => Some((phone, route)),
            _ => None,
        }
    }

    pub fn release_stream(&self, pid: &str) {
        self.conn().release_stream(Some(pid), false);
        self.changed(false);
    }

    pub fn prepare_route(&self, pid: &str, route: &RouteSpec) -> Option<String> {
        let format = self.format();
        self.conn().prepare_stream_route(route, pid, format)
    }

    /// Opens the stream forward to serial ahead of a hand-over, off the UI thread, so prepare_route only counts it.
    pub fn hold_stream_forward(&self, serial: &str) -> bool {
        let tunnels = self.conn().tunnels();
        tunnels.acquire(serial, STREAM_PORT).is_some()
    }

    /// Lets go of hold_stream_forward's hold; the forward goes unless a prepared route still uses it.
    pub fn release_stream_forward(&self, serial: &str) {
        let tunnels = self.conn().tunnels();
        tunnels.release(serial, STREAM_PORT);
    }

    pub fn commit_route(&self, pid: &str) {
        self.conn().commit_stream_route(pid);
        self.changed(false);
    }

    pub fn drop_route(&self, pid: &str, failed: bool) {
        let now = self.now();
        self.conn().drop_stream_route(Some(pid), failed, now);
        self.changed(false);
    }

    /// recovery_probe: the phone and Connect via snapshotted, then checked off the lock.
    pub fn recovery_check(&self, pid: Option<&str>, check: Check) -> Resolution {
        let (phone, preference) = {
            let c = self.conn();
            let pid = pid.map(str::to_string).or_else(|| c.selected_id.clone());
            (pid.and_then(|p| c.phone(Some(&p)).cloned()), c.route_pref)
        };
        match phone {
            Some(phone) => check(&phone, preference),
            None => Resolution::new(PhoneStatus::Unreachable),
        }
    }

    pub fn adopt_route(&self, pid: Option<&str>, route: &RouteSpec) -> Option<String> {
        let format = self.format();
        let url = self.conn().adopt_stream_route(route, pid, format);
        self.changed(false);
        url
    }

    pub fn show_problem(&self, pid: Option<&str>, res: &Resolution) {
        let (pid, save) = {
            let mut c = self.conn();
            let Some(pid) = pid.map(str::to_string).or_else(|| c.selected_id.clone()).filter(|p| c.phone(Some(p)).is_some()) else {
                return;
            };
            let save = c.took_start_check(Some(&pid), res.clone());
            (pid, save)
        };
        self.changed(save);
        self.problem(StartProblem::Unreachable { source_id: pid, resolution: res.clone() });
    }

    pub fn phones(&self) -> Vec<PairedPhone> {
        self.conn().phones.clone()
    }

    pub fn picked_source(&self) -> Option<String> {
        self.conn().selected_id.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connection::VIDEO_H264;
    use std::collections::HashMap;
    use telescope_core::testing::FakeHost;
    use telescope_core::Host;

    #[derive(Default)]
    struct FakeTunnels {
        held: Mutex<HashMap<String, u32>>,
        refuse: bool,
    }

    impl Tunnels for FakeTunnels {
        fn acquire(&self, serial: &str, _remote_port: u16) -> Option<u16> {
            if self.refuse {
                return None;
            }
            *self.held.lock().unwrap().entry(serial.into()).or_insert(0) += 1;
            Some(41000)
        }

        fn release(&self, serial: &str, _remote_port: u16) {
            let mut held = self.held.lock().unwrap();
            let n = held.get_mut(serial).unwrap();
            *n -= 1;
            if *n == 0 {
                held.remove(serial);
            }
        }
    }

    fn phone(pid: &str) -> PairedPhone {
        PairedPhone { id: pid.into(), name: "Pixel".into(), token: "tok".into(), ips: vec!["10.0.0.5".into()], active_ip: None,
                      cert_sha256: "ab".repeat(32) }
    }

    fn with_phone(tunnels: Arc<FakeTunnels>) -> (Arc<Shared>, FakeHost) {
        let shared = Shared::new(tunnels);
        shared.conn().on_phone_paired(phone("id-a"), |_| false);
        shared.conn().mark_selected(Some("id-a"));
        let host = FakeHost::new();
        shared.set_poster(host.poster());
        (shared, host)
    }

    fn ready(route: RouteSpec) -> Resolution {
        Resolution { route: Some(route), ..Resolution::new(PhoneStatus::Ready) }
    }

    fn usb() -> RouteSpec {
        RouteSpec::Usb { serial: "serial-1".into() }
    }

    fn wifi() -> RouteSpec {
        RouteSpec::Network { address: "10.0.0.5".into() }
    }

    const H264: StreamFormat = StreamFormat { h264: true, dynamic: false };

    fn problems(host: &FakeHost) -> Vec<StartProblem> {
        host.take_posted().into_iter().filter(|(t, _)| t == POST_START_PROBLEM)
            .map(|(_, d)| serde_json::from_value(d).unwrap()).collect()
    }

    #[test]
    fn a_start_holds_the_route_the_check_found_and_says_where_the_video_is() {
        let tunnels = Arc::new(FakeTunnels::default());
        let (shared, host) = with_phone(tunnels.clone());
        let asked = Mutex::new(Vec::new());
        let target = shared.start_route(None, H264, &|p, pref| {
            asked.lock().unwrap().push((p.id.clone(), pref));
            ready(usb())
        }).unwrap();
        assert_eq!(asked.into_inner().unwrap(), vec![("id-a".to_string(), RoutePreference::Auto)]);
        assert_eq!(target.video_url, format!("https://127.0.0.1:41000{VIDEO_H264}"));
        assert_eq!(target.route, usb());
        assert_eq!(shared.session_target(Some("id-a")), Some((phone("id-a"), usb())));
        assert_eq!(tunnels.held.lock().unwrap().get("serial-1"), Some(&1));
        assert!(problems(&host).is_empty());
        shared.release_stream("id-a");
        assert!(tunnels.held.lock().unwrap().is_empty());
    }

    #[test]
    fn start_without_a_phone_explains_and_refuses() {
        let shared = Shared::new(Arc::new(FakeTunnels::default()));
        let host = FakeHost::new();
        shared.set_poster(host.poster());
        assert!(shared.start_route(None, H264, &|_, _| panic!("no check without a phone")).is_err());
        assert_eq!(problems(&host), vec![StartProblem::NoPhone]);
    }

    #[test]
    fn start_on_an_unreachable_phone_shows_the_fix() {
        let (shared, host) = with_phone(Arc::new(FakeTunnels::default()));
        let unreachable = Resolution::new(PhoneStatus::Unreachable);
        assert!(shared.start_route(Some("id-a"), H264, &|_, _| unreachable.clone()).is_err());
        assert_eq!(problems(&host), vec![StartProblem::Unreachable { source_id: "id-a".into(), resolution: unreachable.clone() }]);
        // The card shows what the start found
        assert_eq!(shared.conn().resolution, Some(unreachable));
    }

    #[test]
    fn a_forward_adb_refuses_is_its_own_problem() {
        let (shared, host) = with_phone(Arc::new(FakeTunnels { refuse: true, ..Default::default() }));
        assert!(shared.start_route(None, H264, &|_, _| ready(usb())).is_err());
        assert_eq!(problems(&host), vec![StartProblem::Forward { source_id: "id-a".into() }]);
    }

    #[test]
    fn a_hand_over_and_recovery_go_through_the_shared_model() {
        let tunnels = Arc::new(FakeTunnels::default());
        let (shared, host) = with_phone(tunnels.clone());
        shared.start_route(None, H264, &|_, _| ready(wifi())).unwrap();
        let url = shared.prepare_route("id-a", &usb()).unwrap();
        assert!(url.starts_with("https://127.0.0.1:41000"));
        shared.commit_route("id-a");
        assert_eq!(shared.session_target(None).unwrap().1, usb());
        shared.conn().route_pref = RoutePreference::Wifi;
        assert_eq!(shared.recovery_check(None, &|_, pref| if pref == RoutePreference::Wifi { ready(wifi()) } else { panic!() }), ready(wifi()));
        assert_eq!(shared.adopt_route(None, &wifi()).unwrap(), format!("https://10.0.0.5:8080{VIDEO_H264}"));
        assert!(tunnels.held.lock().unwrap().is_empty());
        assert!(host.take_posted().iter().any(|(t, _)| t == POST_CHANGED));
    }

    #[test]
    fn recovery_probe_resolves_with_the_chosen_route() {
        let (shared, _host) = with_phone(Arc::new(FakeTunnels::default()));
        shared.conn().route_pref = RoutePreference::Wifi;
        let calls = Mutex::new(Vec::new());
        let res = Resolution { streaming: true, ..ready(wifi()) };
        assert_eq!(shared.recovery_check(None, &|p, pref| {
            calls.lock().unwrap().push((p.id.clone(), pref));
            res.clone()
        }), res);
        assert_eq!(calls.into_inner().unwrap(), vec![("id-a".to_string(), RoutePreference::Wifi)]);
    }

    #[test]
    fn a_dropped_stream_that_wont_come_back_says_why() {
        let (shared, host) = with_phone(Arc::new(FakeTunnels::default()));
        let res = Resolution::new(PhoneStatus::NotPaired);
        shared.show_problem(None, &res);
        assert_eq!(problems(&host), vec![StartProblem::Unreachable { source_id: "id-a".into(), resolution: res }]);
        shared.show_problem(Some("gone"), &Resolution::new(PhoneStatus::Unreachable));
        assert!(problems(&host).is_empty());
    }
}
