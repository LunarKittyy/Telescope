use super::*;
use std::sync::Mutex;
use telescope_core::output::{Held, Slot};
use telescope_core::testing::{scratch_dir, FakeHost, FakeWeb};
use telescope_core::web::Request;

/// An output set that only has a camera watch to hand out: the callback it was given, and whether it runs.
#[derive(Default)]
struct WatchState {
    on_change: Option<Box<dyn Fn(bool) + Send + Sync>>,
    started: bool,
    stopped: bool,
}

struct FakeWatch(Arc<Mutex<WatchState>>);

impl Held for FakeWatch {
    fn start(&self) {
        self.0.lock().unwrap().started = true;
    }
    fn stop(&self) {
        self.0.lock().unwrap().stopped = true;
    }
    fn running(&self) -> bool {
        self.0.lock().unwrap().started
    }
}

struct FakeOutputs(Arc<Mutex<WatchState>>, Arc<Mutex<u32>>);

impl Outputs for FakeOutputs {
    fn slots(&self) -> Vec<Slot> {
        Vec::new()
    }
    fn formats(&self) -> Vec<telescope_core::frame::PixelFormat> {
        Vec::new()
    }
    fn open(&self, _slot: u32, _width: u32, _height: u32, _fps: u32) -> Result<Box<dyn telescope_core::output::VideoOutput>, String> {
        Err("no camera here".into())
    }
    fn watch(&self, on_change: Box<dyn Fn(bool) + Send + Sync>) -> Option<Box<dyn Held>> {
        *self.1.lock().unwrap() += 1;
        self.0.lock().unwrap().on_change = Some(on_change);
        Some(Box::new(FakeWatch(self.0.clone())))
    }
}

struct Rig {
    plugin: OnAirPlugin,
    host: FakeHost,
    web: Arc<FakeWeb>,
    now: Arc<Mutex<f64>>,
    sent: Arc<Mutex<Vec<WebhookRequest>>>,
    watch: Arc<Mutex<WatchState>>,
    watches: Arc<Mutex<u32>>,
}

impl Rig {
    fn new(cfg: Value) -> Rig {
        Rig::with(cfg, true)
    }

    fn with(cfg: Value, wait_screen: bool) -> Rig {
        let web = Arc::new(FakeWeb::default());
        *web.addresses.lock().unwrap() = vec!["192.168.1.20".into()];
        let (now, sent) = (Arc::new(Mutex::new(100.0)), Arc::new(Mutex::new(Vec::new())));
        let (watch, watches) = (Arc::new(Mutex::new(WatchState::default())), Arc::new(Mutex::new(0)));
        let (clock, record) = (now.clone(), sent.clone());
        let mut plugin = OnAirPlugin::for_test(
            web.clone(), Some(Arc::new(FakeOutputs(watch.clone(), watches.clone()))), scratch_dir("on-air"),
            move || *clock.lock().unwrap(),
            move |request| {
                record.lock().unwrap().push(request.clone());
                String::new()
            },
        );
        let mut host = FakeHost::new();
        if wait_screen {
            host.plugin_configs.insert("wait_screen".into(), json!({}));
        }
        plugin.setup(&mut host);
        plugin.set_config(&cfg).unwrap();
        plugin.on_plugins_ready(&mut host);
        plugin.sender.as_ref().unwrap().wait_idle(Duration::from_secs(2));
        Rig { plugin, host, web, now, sent, watch, watches }
    }

    fn event(&mut self, event: Event) {
        self.plugin.on_event(&mut self.host, &event);
        // What went out goes before the next thing happens, as it would a moment later in life
        self.plugin.sender.as_ref().unwrap().wait_idle(Duration::from_secs(2));
    }

    fn watched(&mut self, watched: bool) {
        self.event(Event::CameraWatched { watched });
    }

    /// Time passes and the settle timer fires.
    fn later(&mut self, seconds: f64) {
        *self.now.lock().unwrap() += seconds;
        self.event(Event::TimerFired { owner: NAME.into(), token: "settle".into() });
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: TAB.into(), callback: callback.into(), args });
    }

    /// Waits for the webhook thread to be through, then gives what it sent.
    fn bodies(&self) -> Vec<String> {
        self.plugin.sender.as_ref().unwrap().wait_idle(Duration::from_secs(2));
        self.sent.lock().unwrap().iter().map(|r| String::from_utf8_lossy(r.body.as_deref().unwrap_or(b"")).into_owned()).collect()
    }

    fn sent_count(&self) -> usize {
        self.plugin.sender.as_ref().unwrap().wait_idle(Duration::from_secs(2));
        self.sent.lock().unwrap().len()
    }

    /// The webhook answers for the state sent last.
    fn answered(&mut self, error: &str) {
        let tag = self.plugin.state_tag();
        self.answer(&tag, error);
    }

    fn answer(&mut self, tag: &str, error: &str) {
        self.event(Event::Posted { owner: NAME.into(), token: "webhook".into(), data: json!({"tag": tag, "error": error}) });
    }

    fn retry_timer(&self) -> Option<u64> {
        self.host.scheduled.iter().find(|(_, t)| t == T_RETRY).map(|(ms, _)| *ms)
    }

    fn fire_retry(&mut self) {
        self.event(Event::TimerFired { owner: NAME.into(), token: "retry".into() });
    }

    fn issue(&self) -> Option<&Issue> {
        self.host.issues.iter().find(|(k, _)| k == ISSUE).map(|(_, i)| i)
    }

    fn saves(&self) -> usize {
        self.host.calls.iter().filter(|c| *c == "schedule_save").count()
    }

    /// What the sign page tells a device that asks (the state it was last handed).
    fn page_state(&self) -> Value {
        let served = self.web.served.lock().unwrap();
        let request = Request {
            method: "GET".into(),
            target: "/state?after=-1".into(),
            headers: vec![("Authorization".into(), format!("Bearer {}", self.plugin.token()))],
        };
        let stats = served[0].stats.clone();
        let reply = served[0].handler.handle(&request, &stats);
        assert_eq!(reply.status, 200);
        serde_json::from_slice(&reply.body).unwrap()
    }

    fn props(&self, ui: &str) -> Value {
        let mut all = serde_json::Map::new();
        let prefix = format!("set_ui_props {ui} ");
        for call in self.host.calls.iter().filter(|c| c.starts_with(&prefix)) {
            let Value::Object(props) = serde_json::from_str::<Value>(&call[prefix.len()..]).unwrap() else { panic!() };
            all.extend(props);
        }
        Value::Object(all)
    }
}

#[test]
fn an_app_watching_the_camera_puts_the_sign_on_after_a_moment() {
    let mut rig = Rig::new(json!({}));
    rig.watched(true);
    assert!(!rig.plugin.on());
    assert!(rig.host.is_scheduled("settle"), "it looks again once the moment is up");
    rig.later(on_air::ON_AFTER_S);
    assert!(rig.plugin.on());
    assert_eq!(rig.page_state()["on"], true);
    rig.watched(false);
    rig.later(2.0);
    assert!(rig.plugin.on(), "still on: an app closing and reopening the camera doesn't flap it");
    rig.later(on_air::OFF_AFTER_S);
    assert!(!rig.plugin.on() && rig.page_state()["on"] == false);
    assert!(!rig.host.is_scheduled("settle"));
}

#[test]
fn the_page_is_told_in_the_current_language() {
    let mut rig = Rig::new(json!({}));
    rig.watched(true);
    rig.later(2.0);
    let state = rig.page_state();
    assert_eq!((state["text"].as_str(), state["title"].as_str()), (Some("On air"), Some("On Air")));
}

#[test]
fn the_mic_trigger_follows_the_mic_while_streaming() {
    let mut rig = Rig::new(json!({"trigger": "camera_mic"}));
    rig.event(Event::MicChanged { enabled: true, muted: false });
    rig.later(2.0);
    assert!(!rig.plugin.on(), "not streaming yet");
    rig.host.streaming = true;
    rig.event(Event::StreamStarted { url: "url".into() });
    rig.later(2.0);
    assert!(rig.plugin.on());
    rig.event(Event::MicChanged { enabled: true, muted: true });
    rig.later(on_air::OFF_AFTER_S + 1.0);
    assert!(!rig.plugin.on());
}

#[test]
fn a_stream_stopping_or_another_starting_is_looked_at_again() {
    let mut rig = Rig::new(json!({"trigger": "streaming"}));
    rig.host.streaming = true;
    rig.event(Event::StreamsChanged { count: 1 });
    rig.later(2.0);
    assert!(rig.plugin.on());
    rig.host.streaming = false;
    rig.event(Event::StreamStopped);
    rig.later(on_air::OFF_AFTER_S + 1.0);
    assert!(!rig.plugin.on());
}

#[test]
fn without_wait_screen_it_watches_the_camera_itself() {
    let mut rig = Rig::with(json!({}), false);
    assert!(rig.watch.lock().unwrap().started);
    // From the watch's thread, through the host's post
    (rig.watch.lock().unwrap().on_change.as_ref().unwrap())(true);
    let posted = rig.host.take_posted();
    assert_eq!(posted.len(), 1);
    let (token, data) = posted.into_iter().next().unwrap();
    rig.event(Event::Posted { owner: NAME.into(), token, data });
    rig.later(2.0);
    assert!(rig.plugin.on());
    rig.plugin.shutdown(&mut rig.host);
    assert!(rig.watch.lock().unwrap().stopped);
}

#[test]
fn with_wait_screen_there_is_no_second_watch() {
    let rig = Rig::new(json!({}));
    assert_eq!(*rig.watches.lock().unwrap(), 0);
}

#[test]
fn each_change_goes_to_the_webhook() {
    let mut rig = Rig::new(json!({"webhook_url": "http://hub.local/hook", "webhook_on": "on", "webhook_off": "off"}));
    rig.watched(true);
    rig.later(2.0);
    rig.watched(false);
    rig.later(on_air::OFF_AFTER_S + 1.0);
    assert_eq!(rig.bodies(), ["off", "on", "off"], "startup first");
}

#[test]
fn no_webhook_without_a_usable_address() {
    let mut rig = Rig::new(json!({"webhook_url": "hub.local/hook"}));
    rig.watched(true);
    rig.later(2.0);
    assert!(rig.plugin.on() && rig.sent_count() == 0);
}

#[test]
fn webhook_failures_make_one_quiet_banner_until_one_works() {
    let mut rig = Rig::new(json!({"webhook_url": "http://hub.local/hook"}));
    rig.answered("Connection refused");
    let first = rig.issue().unwrap().clone();
    assert!(first.kind == "warn" && first.text.contains("Connection refused"));
    rig.host.issues.clear(); // dismissed
    rig.answered("Connection refused");
    assert!(rig.issue().is_none(), "not again for the same run of failures");
    rig.answered("");
    rig.answered("HTTP 500");
    assert!(rig.issue().is_some());
    rig.answered("");
    assert!(rig.issue().is_none());
    assert_eq!(first.actions[0].does, IssueDoes::OpenSettings { tab: "on_air".into() });
}

#[test]
fn a_test_send_shows_its_result_but_never_a_banner() {
    let mut rig = Rig::new(json!({"webhook_url": "http://hub.local/hook"}));
    rig.ui("test-clicked", vec![json!(true)]);
    assert_eq!(rig.props(TAB)["test-text"], "Sending…");
    rig.answer("test", "HTTP 404");
    let p = rig.props(TAB);
    assert!(p["test-text"].as_str().unwrap().contains("HTTP 404") && p["test-kind"] == "status_err" && rig.issue().is_none());
    rig.ui("test-clicked", vec![json!(false)]);
    rig.answer("test", "");
    assert_eq!((rig.props(TAB)["test-text"].as_str(), rig.props(TAB)["test-kind"].as_str()), (Some("It went through."), Some("status_ok")));
    // Without a usable address there's nothing to send
    rig.ui("url-edited", vec![json!("nope")]);
    let before = rig.sent_count();
    rig.ui("test-clicked", vec![json!(true)]);
    assert_eq!(rig.props(TAB)["test-text"], "Enter an address starting with http:// or https:// first.");
    assert_eq!(rig.sent_count(), before);
}

#[test]
fn the_link_survives_a_restart_and_a_new_link_replaces_it() {
    let rig = Rig::new(json!({}));
    let token = rig.plugin.token().to_string();
    assert!(rig.saves() > 0, "a link made fresh is saved at once");
    let cfg = rig.plugin.get_config();
    let mut again = Rig::new(cfg);
    assert_eq!(again.plugin.token(), token);
    assert_eq!(again.plugin.url(), format!("https://192.168.1.20:8768/#{token}"));
    again.ui("new-link-clicked", vec![]);
    assert_ne!(again.plugin.token(), token);
    assert_eq!(again.plugin.url(), format!("https://192.168.1.20:8768/#{}", again.plugin.token()));
    // A device on the old link is refused from its next request on
    let served = again.web.served.lock().unwrap();
    let old = Request { method: "GET".into(), target: "/state?after=-1".into(), headers: vec![("Authorization".into(), format!("Bearer {token}"))] };
    assert_eq!(served[0].handler.handle(&old, &served[0].stats).status, 403);
}

#[test]
fn the_certificate_is_asked_for_with_this_computers_addresses() {
    let rig = Rig::new(json!({}));
    let asked = rig.web.certificates.lock().unwrap();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].1, ["192.168.1.20"]);
    let served = rig.web.served.lock().unwrap();
    assert_eq!((served[0].serve.port, served[0].serve.host.as_str()), (8768, ""));
}

#[test]
fn a_sign_that_wont_start_says_why() {
    let web = FakeWeb::default();
    *web.certificate_error.lock().unwrap() = Some("no disk".into());
    let mut plugin = OnAirPlugin::for_test(Arc::new(web), None, scratch_dir("on-air"), || 0.0, |_| String::new());
    let mut host = FakeHost::new();
    plugin.setup(&mut host);
    plugin.set_config(&json!({})).unwrap();
    plugin.on_plugins_ready(&mut host);
    assert_eq!(plugin.url(), "");
    let sections = plugin.settings_sections();
    assert_eq!(sections[0].ui.props["problem"], "Couldn't start: no disk");
    assert_eq!(plugin.diagnostics()[2].1, "not running");
}

#[test]
fn switching_the_page_off_stops_the_server() {
    let mut rig = Rig::new(json!({}));
    rig.ui("page-toggled", vec![json!(false)]);
    assert_eq!(rig.web.served.lock().unwrap()[0].stopped.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert_eq!(rig.plugin.url(), "");
    assert_eq!(rig.plugin.diagnostics()[2].1, "off");
    assert_eq!(Rig::new(json!({"page": false})).web.served.lock().unwrap().len(), 0);
    // And on again starts one
    rig.ui("page-toggled", vec![json!(true)]);
    assert!(rig.plugin.url().starts_with("https://192.168.1.20:"));
    assert_eq!(rig.web.served.lock().unwrap().len(), 2);
}

#[test]
fn bad_config_falls_back_to_the_defaults() {
    let rig = Rig::new(json!({"trigger": "moon", "token": "x", "webhook_method": "PUT", "webhook_url": 5}));
    let cfg = rig.plugin.get_config();
    assert_eq!((&cfg["trigger"], &cfg["webhook_method"], &cfg["webhook_url"]), (&json!("camera"), &json!("POST"), &json!("")));
    assert!(rig.plugin.token().len() >= 16);
}

#[test]
fn the_settings_section_and_the_header_follow_the_state() {
    let mut rig = Rig::new(json!({"webhook_url": "http://hub.local/hook"}));
    let section = rig.plugin.settings_sections().remove(0);
    assert_eq!((section.tab.as_str(), section.tab_title.as_str()), ("on_air", "On Air"));
    assert_eq!(rig.plugin.header_widget().unwrap().props["shown"], false);
    rig.ui("shown", vec![]);
    rig.watched(true);
    rig.later(2.0);
    assert_eq!(rig.props("header")["shown"], true);
    let p = rig.props(TAB);
    assert_eq!((p["state-text"].as_str(), p["state-kind"].as_str()), (Some("On air"), Some("status_err")));
    assert_eq!(p["url-text"], rig.plugin.url().as_str());
    assert!(p["qr-size"].as_u64().unwrap() > 20 && !p["qr-cells"].as_array().unwrap().is_empty());
    rig.ui("url-edited", vec![json!("ftp://nope")]);
    assert_eq!(rig.plugin.get_config()["webhook_url"], "ftp://nope");
    assert!(!rig.props(TAB)["url-problem"].as_str().unwrap().is_empty());
    rig.ui("method-picked", vec![json!(1)]);
    assert_eq!(rig.props(TAB)["on-placeholder"], "on_air=true");
    assert_eq!(rig.plugin.get_config()["webhook_method"], "GET");
    // The header's button opens the tab
    rig.plugin.on_ui(&mut rig.host, &UiEvent { ui: "header".into(), callback: "clicked".into(), args: vec![] });
    assert!(rig.host.calls.contains(&"open_settings on_air".to_string()));
}

#[test]
fn the_link_can_be_copied() {
    let mut rig = Rig::new(json!({}));
    rig.ui("copy-clicked", vec![]);
    let url = rig.plugin.url();
    assert!(rig.host.calls.contains(&format!("copy_text {url}")));
}

#[test]
fn quitting_while_on_air_turns_the_light_off() {
    let mut rig = Rig::new(json!({"webhook_url": "http://hub.local/hook"}));
    rig.watched(true);
    rig.later(2.0);
    rig.bodies();
    rig.sent.lock().unwrap().clear();
    rig.plugin.shutdown(&mut rig.host);
    assert_eq!(rig.sent.lock().unwrap().iter().map(|r| r.body.clone().unwrap()).collect::<Vec<_>>(), [b"{\"on_air\": false}".to_vec()]);
    assert_eq!(rig.web.served.lock().unwrap()[0].stopped.load(std::sync::atomic::Ordering::SeqCst), 1);
}

#[test]
fn quitting_off_air_sends_nothing() {
    let mut rig = Rig::new(json!({"webhook_url": "http://hub.local/hook"}));
    rig.bodies();
    rig.sent.lock().unwrap().clear();
    rig.plugin.shutdown(&mut rig.host);
    assert_eq!(rig.sent_count(), 0);
}

#[test]
fn quitting_with_a_failure_waiting_to_be_retried_still_turns_the_light_off() {
    let mut rig = Rig::new(json!({"webhook_url": "http://hub.local/hook"}));
    rig.answered("Connection refused");
    rig.bodies();
    rig.sent.lock().unwrap().clear();
    rig.plugin.shutdown(&mut rig.host);
    assert_eq!(rig.sent_count(), 1);
}

#[test]
fn a_failed_state_is_sent_again_with_backoff_until_it_works() {
    let mut rig = Rig::new(json!({"webhook_url": "http://hub.local/hook"}));
    let tag = rig.plugin.state_tag();
    let mut delays = Vec::new();
    for _ in 0..6 {
        rig.answered("Connection refused");
        delays.push(rig.retry_timer().unwrap() / 1000);
        rig.fire_retry(); // the retry goes out
        assert_eq!(rig.plugin.state_tag(), tag, "always the same change, sent again");
    }
    assert_eq!(delays, [5, 15, 60, 300, 300, 300]);
    assert_eq!(rig.sent_count(), 7);
    rig.answered("");
    assert!(rig.retry_timer().is_none() && rig.issue().is_none());
}

#[test]
fn clearing_the_address_ends_the_retries_and_the_banner() {
    let mut rig = Rig::new(json!({"webhook_url": "http://hub.local/hook"}));
    rig.answered("Connection refused");
    assert!(rig.issue().is_some() && rig.retry_timer().is_some());
    rig.ui("url-edited", vec![json!("")]);
    assert!(rig.issue().is_none() && rig.retry_timer().is_none());
    assert_eq!(rig.plugin.diagnostics()[3].1, "not set");
}

#[test]
fn an_address_half_typed_is_a_problem_not_a_crash() {
    let mut rig = Rig::new(json!({}));
    rig.ui("url-edited", vec![json!("http://[")]);
    assert!(!rig.plugin.webhook_ready());
    assert!(!rig.props(TAB)["url-problem"].as_str().unwrap().is_empty());
}

#[test]
fn a_newer_change_replaces_the_retries() {
    let mut rig = Rig::new(json!({"webhook_url": "http://hub.local/hook"}));
    let old = rig.plugin.state_tag();
    rig.answer(&old, "Connection refused");
    assert!(rig.retry_timer().is_some());
    rig.watched(true);
    rig.later(2.0);
    assert!(rig.retry_timer().is_none() && rig.plugin.state_tag() != old);
    rig.answer(&old, "Connection refused"); // a late answer about the old one changes nothing
    assert!(rig.retry_timer().is_none());
}

#[test]
fn the_link_follows_a_network_change() {
    let mut rig = Rig::new(json!({}));
    *rig.web.addresses.lock().unwrap() = vec!["10.0.0.7".into()];
    let saves = rig.saves();
    rig.ui("shown", vec![]);
    assert!(rig.plugin.url().starts_with("https://10.0.0.7:") && rig.saves() > saves);
}

#[test]
fn several_addresses_offer_a_choice_and_a_pick_moves_the_link() {
    let mut rig = Rig::new(json!({}));
    *rig.web.addresses.lock().unwrap() = vec!["192.168.1.20".into(), "100.90.1.2".into()];
    rig.ui("shown", vec![]);
    let p = rig.props(TAB);
    assert_eq!((p["address-shown"].as_bool(), p["addresses"].as_array().unwrap().len()), (Some(true), 2));
    rig.ui("address-picked", vec![json!(1)]);
    assert!(rig.plugin.url().starts_with("https://100.90.1.2:"));
    assert_eq!(rig.plugin.get_config()["address"], "100.90.1.2");
}

#[test]
fn the_trigger_is_picked_from_the_list() {
    let mut rig = Rig::new(json!({}));
    rig.host.streaming = true;
    rig.ui("trigger-picked", vec![json!(3)]);
    assert_eq!(rig.plugin.get_config()["trigger"], "streaming");
    rig.later(2.0);
    assert!(rig.plugin.on(), "the new choice is looked at straight away");
}

#[test]
fn what_it_says_in_diagnostics() {
    let rig = Rig::new(json!({"webhook_url": "http://hub.local/hook", "webhook_method": "GET"}));
    let d: std::collections::HashMap<_, _> = rig.plugin.diagnostics().into_iter().collect();
    assert_eq!((d["On Air"].as_str(), d["On Air when"].as_str()), ("off", "camera"));
    assert_eq!((d["On Air sign page"].as_str(), d["On Air webhook"].as_str()), ("on, port 8768", "set (GET)"));
}
