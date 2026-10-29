//! A stream's camera controls (phone_client.py): sent from a queue on a thread of their own, coalesced by action,
//! sent again on a stalling link, and checked against the phone's /v1/state so a setting it lost goes out again.

use crate::https::{Error, PhoneAuth, Request, MAX_REPLY_BYTES};
use crate::session_client::truthy;
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use telescope_core::link::PhoneControl;

/// Each Next or Previous counts, so these go out one by one.
const NON_COALESCING: &[&str] = &["camera", "notes_scroll"];
/// What a phone that restarted its stream has forgotten, and gets again from resend_settings().
const SETTINGS: &[&str] = &["auto", "iso", "shutter", "wb_auto", "wb_gains", "ois", "focus_mode", "focus_distance", "ae_comp",
                            "nr_mode", "edge_mode", "black_level_lock", "jpeg_quality", "bitrate", "fps_target", "zoom", "low_light"];

/// A request lost on a stalling link is sent again, a little later each time, for this long. Every action sets a
/// value, so one that did arrive and only lost its reply does no harm arriving twice.
#[derive(Clone, Copy, Debug)]
pub struct Retry {
    pub for_: Duration,
    pub first_wait: Duration,
    pub max_wait: Duration,
}

impl Default for Retry {
    fn default() -> Self {
        Retry { for_: Duration::from_secs(15), first_wait: Duration::from_millis(250), max_wait: Duration::from_secs(2) }
    }
}

fn get<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    v.as_object().and_then(|o| o.get(key))
}

/// int(value) as Python reads it; None where Python raises.
fn py_int(v: Option<&Value>) -> Option<i64> {
    match v? {
        Value::Bool(b) => Some(*b as i64),
        Value::Number(n) => n.as_i64().or_else(|| n.as_f64().filter(|f| f.is_finite()).map(|f| f.trunc() as i64)),
        Value::String(s) => s.trim().replace('_', "").parse().ok(),
        _ => None,
    }
}

fn py_float(v: Option<&Value>) -> Option<f64> {
    match v? {
        Value::Bool(b) => Some(*b as i64 as f64),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

/// str(value) == "1", how the switches' values are read.
fn is_one(v: Option<&Value>) -> bool {
    match v {
        Some(Value::String(s)) => s == "1",
        Some(Value::Number(n)) => !n.is_f64() && n.as_i64() == Some(1),
        _ => false,
    }
}

/// state.get(key, default): the default only when the key is missing.
fn state_or<'a>(state: &'a Value, key: &str, default: &'a Value) -> &'a Value {
    get(state, key).unwrap_or(default)
}

/// The sent settings (action and params, in the order sent) that the phone's /v1/state doesn't show.
pub fn drifted_settings(settings: &[(String, Value)], state: &Value) -> Vec<Value> {
    if get(state, "cameras").is_none() {
        return Vec::new(); // not a whole state
    }
    let find = |action: &str| settings.iter().find(|(a, _)| a == action).map(|(_, p)| p);
    let index = |action: &str| settings.iter().position(|(a, _)| a == action);
    let after = |a: &str, b: &str| index(b).is_none() || index(a) > index(b);
    let mut out: Vec<Value> = Vec::new();
    // matches says whether the state shows the value; None is a value or state it can't read, which says nothing
    let mut check = |action: &str, matches: &dyn Fn(Option<&Value>) -> Option<bool>| {
        if let Some(params) = find(action) {
            if matches(get(params, "value")) == Some(false) {
                out.push(params.clone());
            }
        }
    };
    let (t, f, zero, one) = (Value::Bool(true), Value::Bool(false), Value::from(0), Value::from(1));
    let auto_on = truthy(Some(state_or(state, "auto", &t)));
    let mut first: Vec<Value> = Vec::new();
    match (find("iso"), find("shutter")) {
        (Some(iso), Some(shutter)) if truthy(Some(iso)) && truthy(Some(shutter)) && after("iso", "auto") && after("shutter", "auto") => {
            let kept = if auto_on {
                false
            } else {
                let or_zero = |k: &str| get(state, k).filter(|v| truthy(Some(v))).cloned().unwrap_or(Value::from(0));
                match (py_int(Some(&or_zero("iso"))), py_int(get(iso, "value")), py_int(Some(&or_zero("shutter_ns"))), py_int(get(shutter, "value"))) {
                    (Some(a), Some(b), Some(c), Some(d)) => a == b && c == d,
                    _ => true,
                }
            };
            if !kept {
                first.extend([iso.clone(), shutter.clone()]);
            }
        }
        _ => {
            if let Some(auto) = find("auto").filter(|_| !auto_on) {
                first.push(auto.clone());
            }
        }
    }
    let wb_manual = truthy(get(state, "wb_manual"));
    if find("wb_gains").is_some() && after("wb_gains", "wb_auto") {
        check("wb_gains", &|_| Some(wb_manual));
    } else {
        check("wb_auto", &|_| Some(!wb_manual));
    }
    let focus = get(state, "focus_mode").and_then(Value::as_str).filter(|f| *f == "continuous" || *f == "manual");
    if let Some(focus) = focus {
        check("focus_mode", &|v| Some(v == Some(&Value::from(focus))));
        if focus == "manual" {
            let zero_f = Value::from(0.0);
            let have = py_float(Some(state_or(state, "focus_distance", &zero_f)));
            check("focus_distance", &|v| Some((have? - py_float(v)?).abs() < 0.01));
        }
    }
    let int_check = |key: &str, default: &Value| {
        let have = py_int(Some(state_or(state, key, default)));
        move |v: Option<&Value>| Some(have? == py_int(v)?)
    };
    if auto_on {
        check("ae_comp", &int_check("ae_comp", &zero));
    }
    check("fps_target", &int_check("phone_fps", &zero));
    check("jpeg_quality", &int_check("jpeg_quality", &zero));
    let ois = truthy(Some(state_or(state, "ois", &t)));
    check("ois", &|v| Some(ois == is_one(v)));
    let lock = truthy(Some(state_or(state, "black_level_lock", &f)));
    check("black_level_lock", &|v| Some(lock == is_one(v)));
    check("nr_mode", &int_check("nr_mode", &one));
    check("edge_mode", &int_check("edge_mode", &one));
    if truthy(get(state, "low_light_toggle")) {
        // An older phone doesn't know it: sending it again wouldn't change that
        let low = truthy(Some(state_or(state, "low_light", &f)));
        check("low_light", &|v| Some(low == is_one(v)));
    }
    first.extend(out);
    first
}

enum Item {
    /// Sent as it is, in order.
    Each(Value),
    /// The latest value for this action, if seq is still its place in the queue.
    Latest(Option<String>, u64),
    Stop,
}

#[derive(Default)]
struct State {
    queue: VecDeque<Item>,
    /// action: (place in the queue, params), the latest of each.
    pending: HashMap<Option<String>, (u64, Value)>,
    seq: u64,
    /// The last of each setting sent, in the order they were last sent.
    settings: Vec<(String, Value)>,
    /// action: params sent again because the phone's state didn't show them, once each.
    resent: HashMap<String, Value>,
}

pub type Sender = Box<dyn Fn(&str, &Value) -> bool + Send + Sync>;

struct Inner {
    base: Mutex<String>,
    auth: PhoneAuth,
    state: Mutex<State>,
    queued: Condvar,
    closed: AtomicBool,
    /// Wakes a wait between tries when it's closed.
    wake: (Mutex<bool>, Condvar),
    retry: Retry,
    sender: Sender,
}

/// Sends authenticated camera-control requests from a queue on its own thread, coalesced by action.
#[derive(Clone)]
pub struct ControlClient {
    inner: Arc<Inner>,
}

/// The base its requests go to: the stream URL without its "/video..." (rsplit, like Python).
pub fn base_of(stream_url: &str) -> String {
    match stream_url.rfind("/video") {
        Some(i) => stream_url[..i].to_string(),
        None => stream_url.to_string(),
    }
}

impl ControlClient {
    pub fn new(stream_url: &str, auth: PhoneAuth) -> ControlClient {
        let client = ControlClient::unstarted(stream_url, auth, Retry::default(), None);
        let worker = client.clone();
        std::thread::Builder::new().name("phone-control".into()).spawn(move || worker.run_worker()).expect("control thread");
        client
    }

    /// One whose worker the caller runs (tests), with its own retry timing and sender.
    pub fn unstarted(stream_url: &str, auth: PhoneAuth, retry: Retry, sender: Option<Sender>) -> ControlClient {
        let sender = sender.unwrap_or_else(|| {
            let auth = auth.clone();
            Box::new(move |base: &str, params: &Value| send_now(&auth, base, params))
        });
        ControlClient { inner: Arc::new(Inner {
            base: Mutex::new(base_of(stream_url)), auth, state: Mutex::default(), queued: Condvar::new(),
            closed: AtomicBool::new(false), wake: (Mutex::new(false), Condvar::new()), retry, sender,
        }) }
    }

    pub fn base(&self) -> String {
        self.inner.base.lock().unwrap().clone()
    }

    /// The same stream on a new route (cable pulled: Wi-Fi): what's queued, and what resend_settings() sends, goes there.
    pub fn retarget_url(&self, stream_url: &str) {
        *self.inner.base.lock().unwrap() = base_of(stream_url);
    }

    pub fn send(&self, params: &Value) {
        if self.closed() {
            return;
        }
        let action = get(params, "action").and_then(Value::as_str).map(str::to_string);
        let mut st = self.inner.state.lock().unwrap();
        if let Some(a) = &action {
            st.resent.remove(a);
            if SETTINGS.contains(&a.as_str()) {
                st.settings.retain(|(s, _)| s != a);
                st.settings.push((a.clone(), params.clone()));
            }
        }
        if action.as_deref().is_some_and(|a| NON_COALESCING.contains(&a)) {
            st.queue.push_back(Item::Each(params.clone()));
        } else {
            // Goes out at the latest send's place, after whatever was sent between (Manual, Auto, Manual: Manual)
            st.seq += 1;
            let seq = st.seq;
            st.pending.insert(action.clone(), (seq, params.clone()));
            st.queue.push_back(Item::Latest(action, seq));
        }
        self.inner.queued.notify_one();
    }

    /// The phone's /v1/state, or None; sends again a setting the state doesn't show.
    pub fn get_state(&self) -> Option<Value> {
        let url = format!("{}/state", self.base());
        let state = self.inner.auth.open(Request::get(&url, Duration::from_secs(4))).ok()?.read_capped(MAX_REPLY_BYTES).ok()
            .and_then(|raw| serde_json::from_slice::<Value>(&raw).ok())?;
        // Every caller reads it as an object
        if !state.is_object() {
            return None;
        }
        self.resync(&state);
        Some(state)
    }

    /// Sends a setting again when the phone's state doesn't show it; once per value, as the phone may clamp it.
    pub fn resync(&self, state: &Value) {
        let stale: Vec<Value> = {
            let st = self.inner.state.lock().unwrap();
            drifted_settings(&st.settings, state).into_iter().filter(|p| {
                let action = get(p, "action").and_then(Value::as_str).map(str::to_string);
                !st.pending.contains_key(&action) && action.as_ref().and_then(|a| st.resent.get(a)) != Some(p)
            }).collect()
        };
        for params in stale {
            let action = get(&params, "action").and_then(Value::as_str).unwrap_or("").to_string();
            log::info!("The phone doesn't show the {action} sent, sending it again");
            self.send(&params);
            self.inner.state.lock().unwrap().resent.insert(action, params);
        }
    }

    /// Sends the phone every setting again, for a stream that came back while the panels showed another.
    pub fn resend_settings(&self) {
        let settings: Vec<Value> = self.inner.state.lock().unwrap().settings.iter().map(|(_, p)| p.clone()).collect();
        for params in settings {
            self.send(&params);
        }
    }

    /// The settings resend_settings() would send, in order.
    pub fn settings(&self) -> Vec<Value> {
        self.inner.state.lock().unwrap().settings.iter().map(|(_, p)| p.clone()).collect()
    }

    pub fn closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// Stops taking requests and drops queued ones (the stream is over).
    pub fn close(&self) {
        if self.inner.closed.swap(true, Ordering::SeqCst) {
            return;
        }
        *self.inner.wake.0.lock().unwrap() = true;
        self.inner.wake.1.notify_all();
        let mut st = self.inner.state.lock().unwrap();
        st.pending.clear();
        st.queue.clear();
        st.queue.push_back(Item::Stop);
        self.inner.queued.notify_all();
    }

    /// Takes everything queued now without sending it (tests).
    pub fn drain_queue(&self) -> Vec<Option<String>> {
        let mut st = self.inner.state.lock().unwrap();
        st.queue.drain(..).map(|item| match item {
            Item::Each(p) => get(&p, "action").and_then(Value::as_str).map(str::to_string),
            Item::Latest(a, _) => a,
            Item::Stop => None,
        }).collect()
    }

    /// Forgets which actions wait in the queue, as if they'd all gone out (tests).
    pub fn forget_pending(&self) {
        self.inner.state.lock().unwrap().pending.clear();
    }

    /// Puts the end marker in the queue, so run_worker() returns once it gets there (tests).
    pub fn end_queue(&self) {
        self.inner.state.lock().unwrap().queue.push_back(Item::Stop);
        self.inner.queued.notify_all();
    }

    /// The worker: takes the queue in order until it's closed.
    pub fn run_worker(&self) {
        loop {
            let params = {
                let mut st = self.inner.state.lock().unwrap();
                loop {
                    if let Some(item) = st.queue.pop_front() {
                        break item;
                    }
                    st = self.inner.queued.wait(st).unwrap();
                }
            };
            let params = match params {
                Item::Stop => return,
                Item::Each(p) => p,
                Item::Latest(action, seq) => {
                    let mut st = self.inner.state.lock().unwrap();
                    match st.pending.get(&action) {
                        // Sent already, or a newer value waits further back
                        Some((queued, _)) if *queued == seq => st.pending.remove(&action).unwrap().1,
                        _ => continue,
                    }
                }
            };
            self.deliver(&params);
        }
    }

    /// Sends until it gets through, the phone refuses it, a newer value for the same action waits, or time's up.
    pub fn deliver(&self, params: &Value) {
        let action = get(params, "action").and_then(Value::as_str).map(str::to_string);
        let retry = self.inner.retry;
        let deadline = Instant::now() + retry.for_;
        let mut wait = retry.first_wait;
        while !(self.inner.sender)(&self.base(), params) {
            let superseded = self.inner.state.lock().unwrap().pending.contains_key(&action);
            if self.closed() || superseded {
                return;
            }
            if Instant::now() + wait > deadline {
                log::warn!("Gave up sending {} to the phone", action.as_deref().unwrap_or("?"));
                return;
            }
            let (lock, cv) = &self.inner.wake;
            let closed = cv.wait_timeout_while(lock.lock().unwrap(), wait, |closed| !*closed).unwrap().0;
            if *closed {
                return; // closed: this phone, or this session with it, is done
            }
            wait = (wait * 2).min(retry.max_wait);
        }
    }
}

/// One POST /control; false when it may not have got there, so it's worth sending again.
pub fn send_now(auth: &PhoneAuth, base: &str, params: &Value) -> bool {
    let body = crate::util::python_json_utf8(params);
    let url = format!("{base}/control");
    match auth.open(Request::post(&url, body.as_bytes(), Duration::from_secs(3))) {
        // A reply cut off on the way may not have been acted on either
        Ok(mut r) => match std::io::copy(&mut r, &mut std::io::sink()) {
            Ok(_) => true,
            Err(e) => {
                log::debug!("Control request failed: {e}");
                false
            }
        },
        Err(Error::Http(code)) => {
            log::debug!("The phone refused a control request: HTTP {code}"); // it got there: again won't change that
            true
        }
        Err(e) => {
            log::debug!("Control request failed: {e}");
            false
        }
    }
}

impl PhoneControl for ControlClient {
    fn send(&self, body: &Value) {
        ControlClient::send(self, body);
    }

    fn get_state(&self) -> Option<Value> {
        ControlClient::get_state(self)
    }

    fn retarget(&self, stream_base: &str) {
        *self.inner.base.lock().unwrap() = stream_base.to_string();
    }

    fn resend_settings(&self) {
        ControlClient::resend_settings(self);
    }

    fn close(&self) {
        ControlClient::close(self);
    }

    fn closed(&self) -> bool {
        ControlClient::closed(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::https::testing::*;
    use serde_json::json;
    use std::sync::Mutex;

    fn quick() -> Retry {
        Retry { for_: Duration::from_secs(15), first_wait: Duration::from_millis(10), max_wait: Duration::from_millis(20) }
    }

    /// A client whose sends are recorded and go by a script: true got there, false was lost on the way.
    fn scripted(outcomes: Vec<bool>, retry: Retry) -> (ControlClient, Arc<Mutex<Vec<Value>>>) {
        let tries: Arc<Mutex<Vec<Value>>> = Arc::default();
        let (record, script) = (tries.clone(), Mutex::new(VecDeque::from(outcomes)));
        let sender: Sender = Box::new(move |_base, params| {
            record.lock().unwrap().push(params.clone());
            script.lock().unwrap().pop_front().unwrap_or(false)
        });
        (ControlClient::unstarted("http://phone/video", PhoneAuth::new("tok", ""), retry, Some(sender)), tries)
    }

    /// A phone recording each control; reply gates its answers.
    struct Recorder {
        server: Server,
        gate: Arc<(Mutex<bool>, Condvar)>,
    }

    impl Recorder {
        fn new() -> Recorder {
            let gate: Arc<(Mutex<bool>, Condvar)> = Arc::new((Mutex::new(true), Condvar::new()));
            let g = gate.clone();
            let server = serve(None, Arc::new(move |_req| {
                let (lock, cv) = &*g;
                let _open = cv.wait_timeout_while(lock.lock().unwrap(), Duration::from_secs(5), |open| !*open).unwrap();
                (200, "application/json".into(), b"{}".to_vec())
            }));
            Recorder { server, gate }
        }

        fn hold(&self, held: bool) {
            *self.gate.0.lock().unwrap() = !held;
            self.gate.1.notify_all();
        }

        fn received(&self) -> Vec<Value> {
            self.server.seen.lock().unwrap().iter().map(|s| serde_json::from_slice(&s.body).unwrap()).collect()
        }

        fn wait_for(&self, n: usize) {
            let deadline = Instant::now() + Duration::from_secs(5);
            while self.received().len() < n {
                assert!(Instant::now() < deadline, "{:?}", self.received());
                std::thread::sleep(Duration::from_millis(5));
            }
        }

        fn client(&self, token: &str) -> ControlClient {
            ControlClient::new(&format!("{}/video", self.server.base(false)), PhoneAuth::new(token, ""))
        }
    }

    #[test]
    fn rapid_updates_coalesce_to_latest_value() {
        let phone = Recorder::new();
        let client = phone.client("tok");
        phone.hold(true);
        client.send(&json!({"action": "camera", "id": "hold"}));
        phone.wait_for(1);
        for v in [100, 200, 300] {
            client.send(&json!({"action": "iso", "value": v}));
        }
        client.send(&json!({"action": "camera", "id": "after-iso"}));
        phone.hold(false);
        phone.wait_for(3);
        assert_eq!(phone.received(), [json!({"action": "camera", "id": "hold"}), json!({"action": "iso", "value": 300}),
                                      json!({"action": "camera", "id": "after-iso"})]);
        client.close();
    }

    #[test]
    fn non_coalescing_actions_preserve_order() {
        let phone = Recorder::new();
        let client = phone.client("tok");
        phone.hold(true);
        client.send(&json!({"action": "camera", "id": "cam0"}));
        phone.wait_for(1);
        client.send(&json!({"action": "camera", "id": "cam1"}));
        client.send(&json!({"action": "camera", "id": "cam2"}));
        phone.hold(false);
        phone.wait_for(3);
        let ids: Vec<Value> = phone.received().iter().map(|r| r["id"].clone()).collect();
        assert_eq!(ids, [json!("cam0"), json!("cam1"), json!("cam2")]);
        client.close();
    }

    #[test]
    fn every_next_or_previous_reaches_the_phone() {
        let phone = Recorder::new();
        let client = phone.client("tok");
        phone.hold(true);
        client.send(&json!({"action": "camera", "id": "hold"}));
        phone.wait_for(1);
        for by in [1, 1, -1] {
            client.send(&json!({"action": "notes_scroll", "by": by}));
        }
        phone.hold(false);
        phone.wait_for(4);
        let by: Vec<Value> = phone.received()[1..].iter().map(|r| r["by"].clone()).collect();
        assert_eq!(by, [json!(1), json!(1), json!(-1)]);
        client.close();
    }

    #[test]
    fn requests_carry_bearer_token() {
        let phone = Recorder::new();
        let client = phone.client("secret-tok");
        client.send(&json!({"action": "iso", "value": 100}));
        phone.wait_for(1);
        assert_eq!(phone.server.seen.lock().unwrap()[0].auth.as_deref(), Some("Bearer secret-tok"));
        client.close();
    }

    #[test]
    fn close_stops_accepting_new_requests() {
        let phone = Recorder::new();
        let client = phone.client("tok");
        assert!(!client.closed());
        client.close();
        assert!(client.closed());
        client.send(&json!({"action": "iso", "value": 1}));
        std::thread::sleep(Duration::from_millis(50));
        assert!(phone.received().is_empty());
    }

    #[test]
    fn close_cancels_queued_and_pending_requests() {
        let (client, tries) = scripted(vec![], quick());
        client.send(&json!({"action": "iso", "value": 100}));
        client.send(&json!({"action": "camera", "id": "cam0"}));
        client.close();
        client.run_worker();
        assert!(tries.lock().unwrap().is_empty());
    }

    #[test]
    fn resend_settings_sends_the_last_of_each_in_the_order_last_sent() {
        let (client, _) = scripted(vec![], quick());
        client.send(&json!({"action": "iso", "value": 100}));
        client.send(&json!({"action": "wb_auto"}));
        client.send(&json!({"action": "torch", "value": "1"}));
        client.send(&json!({"action": "auto"}));
        client.send(&json!({"action": "zoom", "value": 2.0}));
        assert_eq!(client.settings(), [json!({"action": "iso", "value": 100}), json!({"action": "wb_auto"}), json!({"action": "auto"}),
                                       json!({"action": "zoom", "value": 2.0})]);
        client.drain_queue();
        client.resend_settings();
        assert_eq!(client.drain_queue(), ["iso", "wb_auto", "auto", "zoom"].map(|a| Some(a.to_string())));
    }

    #[test]
    fn base_url_strips_only_trailing_video_component() {
        let auth = PhoneAuth::new("tok", "");
        assert_eq!(ControlClient::unstarted("http://phone/video", auth.clone(), quick(), None).base(), "http://phone");
        assert_eq!(ControlClient::unstarted("http://video-host/path/video", auth, quick(), None).base(), "http://video-host/path");
    }

    fn state_server(status: u16, body: &'static str) -> (Server, ControlClient) {
        let server = serve(None, Arc::new(move |_| (status, "application/json".into(), body.as_bytes().to_vec())));
        let client = ControlClient::unstarted(&format!("{}/video", server.base(false)), PhoneAuth::new("tok123", ""), quick(), None);
        (server, client)
    }

    #[test]
    fn get_state_decodes_json_and_sends_auth_header() {
        let (server, client) = state_server(200, r#"{"battery": 81}"#);
        assert_eq!(client.get_state(), Some(json!({"battery": 81})));
        let seen = server.seen.lock().unwrap();
        assert_eq!((seen[0].path.as_str(), seen[0].auth.as_deref()), ("/state", Some("Bearer tok123")));
    }

    #[test]
    fn get_state_returns_none_on_transport_or_json_error() {
        let offline = ControlClient::unstarted("http://127.0.0.1:1/video", PhoneAuth::new("tok", ""), quick(), None);
        assert_eq!(offline.get_state(), None);
        assert_eq!(state_server(200, "not-json").1.get_state(), None);
    }

    #[test]
    fn get_state_returns_none_for_json_that_is_not_an_object() {
        assert_eq!(state_server(200, "[1, 2]").1.get_state(), None);
    }

    #[test]
    fn send_now_posts_json_body_with_auth_header() {
        let (server, _) = state_server(200, "ok");
        assert!(send_now(&PhoneAuth::new("tok123", ""), &server.base(false), &json!({"action": "camera", "id": "wide angle"})));
        let seen = server.seen.lock().unwrap();
        assert_eq!((seen[0].method.as_str(), seen[0].path.as_str(), seen[0].auth.as_deref()), ("POST", "/control", Some("Bearer tok123")));
        assert_eq!(serde_json::from_slice::<Value>(&seen[0].body).unwrap(), json!({"action": "camera", "id": "wide angle"}));
    }

    #[test]
    fn send_now_sends_text_as_utf8_not_escapes() {
        let (server, _) = state_server(200, "ok");
        send_now(&PhoneAuth::new("tok", ""), &server.base(false), &json!({"action": "notes", "text": "Привет"}));
        let body = server.seen.lock().unwrap()[0].body.clone();
        assert!(String::from_utf8(body).unwrap().contains("Привет"));
    }

    #[test]
    fn send_now_swallows_transport_errors() {
        assert!(!send_now(&PhoneAuth::new("tok", ""), "http://127.0.0.1:1", &json!({"action": "iso", "value": 100})));
    }

    #[test]
    fn a_reply_cut_off_mid_body_is_worth_sending_again() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let (mut conn, _) = listener.accept().unwrap();
            let mut buf = [0u8; 4096];
            let _ = conn.read(&mut buf);
            let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"ok\"");
        });
        assert!(!send_now(&PhoneAuth::new("tok", ""), &format!("http://127.0.0.1:{port}/v1"), &json!({"action": "iso", "value": 100})));
    }

    #[test]
    fn close_is_idempotent() {
        let (client, _) = scripted(vec![], quick());
        client.close();
        client.close();
        assert!(client.closed());
        client.run_worker(); // ends on the one Stop
    }

    #[test]
    fn worker_skips_stale_pending_key() {
        let (client, tries) = scripted(vec![], quick());
        client.inner.state.lock().unwrap().queue.push_back(Item::Latest(Some("iso".into()), 1));
        client.end_queue();
        client.run_worker();
        assert!(tries.lock().unwrap().is_empty());
    }

    #[test]
    fn a_request_lost_on_a_stalling_link_is_sent_again_until_it_gets_through() {
        let (client, tries) = scripted(vec![false, false, true], quick());
        client.deliver(&json!({"action": "iso", "value": 100}));
        assert_eq!(*tries.lock().unwrap(), vec![json!({"action": "iso", "value": 100}); 3]);
    }

    #[test]
    fn a_lost_request_gives_way_to_a_newer_value() {
        let (client, tries) = scripted(vec![false], quick());
        client.send(&json!({"action": "iso", "value": 200}));
        client.deliver(&json!({"action": "iso", "value": 100}));
        assert_eq!(*tries.lock().unwrap(), [json!({"action": "iso", "value": 100})]);
    }

    #[test]
    fn a_lost_request_gives_up_in_the_end() {
        let (client, tries) = scripted(vec![], Retry { for_: Duration::from_millis(100), ..quick() });
        client.deliver(&json!({"action": "iso", "value": 100}));
        let n = tries.lock().unwrap().len();
        assert!((2..20).contains(&n), "{n}");
    }

    #[test]
    fn close_ends_the_retries() {
        let slot: Arc<Mutex<Option<ControlClient>>> = Arc::default();
        let (tries, s) = (Arc::new(Mutex::new(0)), slot.clone());
        let count = tries.clone();
        let sender: Sender = Box::new(move |_, _| {
            *count.lock().unwrap() += 1;
            if let Some(c) = s.lock().unwrap().as_ref() {
                c.close(); // Stop, while the first try hangs on a stalled link
            }
            false
        });
        let client = ControlClient::unstarted("http://phone/video", PhoneAuth::new("tok", ""),
                                              Retry { first_wait: Duration::from_secs(5), ..quick() }, Some(sender));
        *slot.lock().unwrap() = Some(client.clone());
        let started = Instant::now();
        client.deliver(&json!({"action": "camera", "id": "0"}));
        assert!(*tries.lock().unwrap() == 1 && started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn a_request_the_phone_refused_is_not_sent_again() {
        let (server, client) = state_server(400, "bad value");
        client.deliver(&json!({"action": "iso", "value": -5}));
        assert_eq!(server.seen.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_stalled_link_still_delivers_the_setting() {
        let phone = Recorder::new();
        let auth = PhoneAuth::new("tok", "");
        let first = AtomicBool::new(true);
        let real = auth.clone();
        let sender: Sender = Box::new(move |base, params| !first.swap(false, Ordering::SeqCst) && send_now(&real, base, params));
        let client = ControlClient::unstarted(&format!("{}/video", phone.server.base(false)), auth, quick(), Some(sender));
        let worker = client.clone();
        std::thread::spawn(move || worker.run_worker());
        client.send(&json!({"action": "iso", "value": 100}));
        phone.wait_for(1);
        assert_eq!(phone.received(), [json!({"action": "iso", "value": 100})]);
        client.close();
    }

    #[test]
    fn a_value_sent_again_goes_after_what_was_sent_between() {
        let (client, tries) = scripted(vec![true; 10], quick());
        client.send(&json!({"action": "iso", "value": 100}));
        client.send(&json!({"action": "shutter", "value": 10_000_000}));
        client.send(&json!({"action": "auto"}));
        client.send(&json!({"action": "iso", "value": 200}));
        client.send(&json!({"action": "shutter", "value": 20_000_000}));
        client.end_queue();
        client.run_worker();
        assert_eq!(*tries.lock().unwrap(), [json!({"action": "auto"}), json!({"action": "iso", "value": 200}),
                                            json!({"action": "shutter", "value": 20_000_000})]);
    }

    #[test]
    fn a_new_route_takes_the_queue_and_the_settings_along() {
        let (client, _) = scripted(vec![], quick());
        client.retarget_url("http://127.0.0.1:40001/v1/video");
        client.send(&json!({"action": "iso", "value": 500}));
        client.retarget_url("https://192.168.1.20:8080/v1/video");
        assert_eq!(client.base(), "https://192.168.1.20:8080/v1");
        assert_eq!(client.settings(), [json!({"action": "iso", "value": 500})]);
    }

    fn state() -> Value {
        json!({"cameras": [], "auto": true, "wb_manual": false, "focus_mode": "continuous", "focus_distance": 0.0,
               "ae_comp": 0, "phone_fps": 30, "jpeg_quality": 85, "ois": true, "black_level_lock": false, "nr_mode": 1, "edge_mode": 1})
    }

    fn with(extra: Value) -> Value {
        let mut s = state();
        for (k, v) in extra.as_object().unwrap() {
            s[k] = v.clone();
        }
        s
    }

    fn settings(list: &[Value]) -> Vec<(String, Value)> {
        list.iter().map(|p| (p["action"].as_str().unwrap().to_string(), p.clone())).collect()
    }

    #[test]
    fn settings_the_phone_kept_are_left_alone() {
        let s = settings(&[json!({"action": "auto"}), json!({"action": "fps_target", "value": 30}), json!({"action": "ois", "value": "1"}),
                           json!({"action": "nr_mode", "value": 1})]);
        assert!(drifted_settings(&s, &state()).is_empty());
        assert!(drifted_settings(&s, &json!({"battery": 50})).is_empty());
    }

    #[test]
    fn settings_the_phone_lost_are_found() {
        let fps = json!({"action": "fps_target", "value": 30});
        let (iso, shutter) = (json!({"action": "iso", "value": 400}), json!({"action": "shutter", "value": 10_000_000}));
        let lost = with(json!({"phone_fps": 1, "auto": false, "iso": -1, "shutter_ns": i64::MIN}));
        assert_eq!(drifted_settings(&settings(&[json!({"action": "auto"}), fps.clone()]), &lost), [json!({"action": "auto"}), fps]);
        let manual = settings(&[json!({"action": "auto"}), iso.clone(), shutter.clone()]);
        assert_eq!(drifted_settings(&manual, &lost), [iso, shutter]);
        let mut kept = lost.clone();
        kept["iso"] = json!(400);
        kept["shutter_ns"] = json!(10_000_000);
        assert!(drifted_settings(&manual, &kept).is_empty());
    }

    #[test]
    fn point_focus_isnt_taken_for_a_lost_focus_mode() {
        let s = settings(&[json!({"action": "focus_mode", "value": "continuous"})]);
        assert!(drifted_settings(&s, &with(json!({"focus_mode": "point"}))).is_empty());
        assert_eq!(drifted_settings(&s, &with(json!({"focus_mode": "manual"}))), [json!({"action": "focus_mode", "value": "continuous"})]);
    }

    #[test]
    fn a_state_that_doesnt_show_a_setting_sends_it_again_once() {
        let (client, _) = scripted(vec![true; 10], quick());
        client.send(&json!({"action": "fps_target", "value": 30}));
        client.forget_pending();
        client.drain_queue();
        let lost = with(json!({"phone_fps": 1}));
        let mut resent = Vec::new();
        for _ in 0..3 {
            client.resync(&lost);
            resent.extend(client.drain_queue());
            client.forget_pending();
        }
        assert_eq!(resent, [Some("fps_target".to_string())]);
    }

    #[test]
    fn picking_the_value_again_sends_it_again() {
        let (client, _) = scripted(vec![], quick());
        client.send(&json!({"action": "jpeg_quality", "value": 90}));
        client.forget_pending();
        client.resync(&state()); // 85 on the phone: sent again
        client.forget_pending();
        client.resync(&state()); // still 85: left alone
        client.send(&json!({"action": "jpeg_quality", "value": 90}));
        client.forget_pending();
        client.resync(&state()); // a new pick: one more try
        assert_eq!(client.drain_queue(), vec![Some("jpeg_quality".to_string()); 4]);
    }

    #[test]
    fn low_light_is_checked_only_on_a_phone_that_takes_it() {
        let s = settings(&[json!({"action": "low_light", "value": "1"})]);
        assert!(drifted_settings(&s, &state()).is_empty());
        let newer = with(json!({"low_light_toggle": true}));
        assert_eq!(drifted_settings(&s, &newer), [json!({"action": "low_light", "value": "1"})]);
        assert!(drifted_settings(&s, &with(json!({"low_light_toggle": true, "low_light": true}))).is_empty());
    }
}
