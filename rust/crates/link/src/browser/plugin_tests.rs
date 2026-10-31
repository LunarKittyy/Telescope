//! tests/test_browser_camera.py, test for test, the plugin's part: a host that keeps its own list of streams and a
//! stand-in server, as the Python tests' _Host and _Server.

use super::*;
use crate::browser::feed::Sent;
use std::collections::HashMap;

#[derive(Default)]
struct TestHost {
    sources: HashMap<String, Arc<dyn StreamSource>>,
    issues: HashMap<String, Issue>,
    starts: Vec<String>,
    /// Ids, in start order.
    streams: Vec<String>,
    starting: bool,
    saves: u32,
    /// Ids with settings saved.
    settings: Vec<String>,
    forgotten: Vec<String>,
    /// stop_stream calls, waiting tiles included.
    stopped: Vec<String>,
    outputs: Vec<(String, u32)>,
    card: Value,
    events: Vec<Event>,
    copied: String,
    posted: Arc<Mutex<Vec<(String, Value)>>>,
    /// Timers asked for, by token.
    timers: Vec<String>,
}

impl BrowserHost for TestHost {
    fn add_stream_source(&mut self, source: Arc<dyn StreamSource>) {
        self.sources.insert(source.id(), source);
    }
    fn remove_stream_source(&mut self, source_id: &str) {
        self.sources.remove(source_id);
        self.streams.retain(|s| s != source_id);
    }
    fn stream_source(&mut self, source_id: &str) {
        self.starts.push(source_id.into());
        self.streams.push(source_id.into());
    }
    fn stop_stream(&mut self, source_id: &str) {
        self.stopped.push(source_id.into());
        self.streams.retain(|s| s != source_id);
    }
    fn stream_output(&self, source_id: &str) -> String {
        self.streams.iter().position(|s| s == source_id).map_or(String::new(), |i| format!("Camera {}", i + 1))
    }
    fn stream_count(&self) -> u32 {
        self.streams.len() as u32
    }
    fn focused_source_id(&self) -> Option<String> {
        Some(SOURCE_ID.into())
    }
    fn has_device_settings(&self, source_id: &str) -> bool {
        self.settings.iter().any(|s| s == source_id)
    }
    fn forget_device_settings(&mut self, source_id: &str) {
        self.forgotten.push(source_id.into());
        self.settings.retain(|s| s != source_id);
    }
    fn show_issue(&mut self, key: &str, issue: Issue) {
        self.issues.insert(key.into(), issue);
    }
    fn clear_issue(&mut self, key: &str) {
        self.issues.remove(key);
    }
    fn is_streaming_from(&self, source_id: &str) -> bool {
        self.streams.iter().any(|s| s == source_id)
    }
    fn is_starting(&self) -> bool {
        self.starting
    }
    fn set_fps(&mut self, source_id: &str, fps: u32) {
        self.outputs.push((source_id.into(), fps));
    }
    fn schedule_save(&mut self) {
        self.saves += 1;
    }
    fn set_card(&mut self, props: &Value) {
        self.card = props.clone();
    }
    fn emit(&mut self, event: Event) {
        self.events.push(event);
    }
    fn schedule(&mut self, _after_ms: u64, token: &str) {
        self.timers.push(token.into());
    }
    fn copy_text(&mut self, text: &str) {
        self.copied = text.into();
    }
}

struct TestPoster(Arc<Mutex<Vec<(String, Value)>>>);

impl Poster for TestPoster {
    fn post(&self, token: &str, data: Value) {
        self.0.lock().unwrap().push((token.into(), data));
    }
}

/// What the stand-in server went through.
#[derive(Default)]
struct ServerLog {
    started: AtomicBool,
    stopped: AtomicBool,
    token: Mutex<String>,
    /// What stats() says from now on, for a server that's been up a while.
    aged: Mutex<Option<Arc<ServerStats>>>,
}

struct FakeServer {
    log: Arc<ServerLog>,
    stats: Arc<ServerStats>,
}

impl Serving for FakeServer {
    fn start(&self) -> Result<(), String> {
        self.log.started.store(true, Ordering::SeqCst);
        Ok(())
    }
    fn stop(&self) {
        self.log.stopped.store(true, Ordering::SeqCst);
    }
    fn url_for(&self, host: &str) -> String {
        format!("https://{host}:8767/#{}", self.log.token.lock().unwrap())
    }
    fn new_token(&self) {
        *self.log.token.lock().unwrap() = "t2".into();
    }
    fn stats(&self) -> Option<Arc<ServerStats>> {
        Some(self.log.aged.lock().unwrap().clone().unwrap_or_else(|| self.stats.clone()))
    }
}

/// Every server the plugin made, newest last, with the stats each one hands out.
type Servers = Arc<Mutex<Vec<(Arc<ServerLog>, Arc<ServerStats>)>>>;

fn fake_servers(stats: impl Fn() -> ServerStats + Send + 'static) -> (ServerMaker, Servers) {
    let made: Servers = Arc::default();
    let all = made.clone();
    let maker: ServerMaker = Box::new(move |_hub, _on_change| {
        let log = Arc::new(ServerLog { token: Mutex::new("t1".into()), ..ServerLog::default() });
        let stats = Arc::new(stats());
        all.lock().unwrap().push((log.clone(), stats.clone()));
        Ok(Box::new(FakeServer { log, stats }) as Box<dyn Serving>)
    });
    (maker, made)
}

struct Rig {
    plugin: BrowserCameraPlugin,
    host: TestHost,
    servers: Servers,
}

impl Rig {
    fn new() -> Rig {
        Rig::with(|| ServerStats::new(8767, 8767, false, "Browser camera"))
    }

    fn with(stats: impl Fn() -> ServerStats + Send + 'static) -> Rig {
        let (maker, servers) = fake_servers(stats);
        let mut plugin = BrowserCameraPlugin::new(maker, || vec!["192.168.1.20".into(), "100.64.0.3".into()]);
        let mut host = TestHost::default();
        let poster: Arc<dyn Poster> = Arc::new(TestPoster(host.posted.clone()));
        plugin.setup_on(&mut host, Some(poster));
        Rig { plugin, host, servers }
    }

    fn server(&self) -> Arc<ServerLog> {
        self.servers.lock().unwrap().last().unwrap().0.clone()
    }

    fn stats(&self) -> Arc<ServerStats> {
        self.servers.lock().unwrap().last().unwrap().1.clone()
    }

    /// What the sources posted reaches the plugin, as the host would deliver it.
    fn pump(&mut self) {
        let posted: Vec<(String, Value)> = std::mem::take(&mut *self.host.posted.lock().unwrap());
        for (token, data) in posted {
            self.plugin.hear(&mut self.host, &Event::Posted { owner: NAME.into(), token, data });
        }
    }

    fn select(&mut self, sid: &str) {
        self.plugin.hear(&mut self.host, &Event::SourceSelected { source_id: sid.into() });
    }

    fn changed(&mut self) {
        let count = self.host.streams.len() as u32;
        self.plugin.hear(&mut self.host, &Event::StreamsChanged { count });
    }

    /// A browser connects (and says what it runs on), as the server's thread would report it.
    fn arrive(&mut self, bid: &str, device: &str) -> (Arc<BrowserFeed>, u64) {
        let feed = self.plugin.hub.join(bid).unwrap();
        let gen = feed.connect();
        if !device.is_empty() {
            feed.note_hello(gen, device, "");
        }
        self.plugin.on_server_changed(&mut self.host);
        (feed, gen)
    }

    fn launcher(&self) -> Arc<dyn StreamSource> {
        self.host.sources[SOURCE_ID].clone()
    }

    fn card(&self) -> Value {
        self.plugin.props(&self.host)
    }

    fn name_of(&self, sid: &str) -> String {
        self.host.sources[sid].name()
    }

    fn browser_sources(&self) -> Vec<String> {
        self.host.sources.keys().filter(|k| k.starts_with(BROWSER_PREFIX)).cloned().collect()
    }
}

fn camera(feed: &BrowserFeed) -> Value {
    feed.page_config().1["camera"].clone()
}

#[test]
fn offers_a_picker_entry_that_waits_for_a_browser() {
    let mut r = Rig::new();
    let launcher = r.launcher();
    assert!(launcher.name() == "Browser camera" && launcher.redirect().is_none());
    r.select(SOURCE_ID);
    assert!(!launcher.prepare(false)); // nothing to stream yet: the first browser to connect starts
    r.pump();
    assert_eq!(r.host.issues["start"].title, "Waiting for a browser");
}

#[test]
fn picking_it_runs_the_server_and_shows_the_code() {
    let mut r = Rig::new();
    assert_eq!(r.card()["shown"], false);
    r.select(SOURCE_ID);
    let server = r.server();
    assert!(server.started.load(Ordering::SeqCst));
    let card = r.card();
    assert_eq!(card["shown"], true);
    assert_eq!(card["url"], "https://192.168.1.20:8767/#t1");
    assert!(card["qr-size"].as_u64().unwrap() > 0);
    assert_eq!(card["show-addresses"], true); // two addresses to choose from

    r.plugin.new_link(&mut r.host);
    assert!(r.card()["url"].as_str().unwrap().ends_with("#t2"));

    r.select("some-phone");
    assert!(server.stopped.load(Ordering::SeqCst) && r.card()["shown"] == false);
}

#[test]
fn each_browser_streams_by_itself_to_a_camera_of_its_own() {
    let mut r = Rig::new();
    r.select(SOURCE_ID);
    let (pixel, _) = r.arrive("pixel-aaaaaaaa", "Chrome on Android");
    assert_eq!(r.host.starts, ["browser:pixel-aaaaaaaa"]); // like opening the app on a phone
    assert_eq!(r.name_of("browser:pixel-aaaaaaaa"), "Chrome on Android");
    assert!(!r.host.issues.contains_key("start"));
    r.changed();
    assert_eq!(camera(&pixel), "Camera 1");

    let (iphone, _) = r.arrive("iphone-bbbbbbbb", "Safari on iPhone");
    assert_eq!(r.host.starts, ["browser:pixel-aaaaaaaa", "browser:iphone-bbbbbbbb"]);
    r.changed();
    assert_eq!(camera(&iphone), "Camera 2");
    let card = r.card();
    assert_eq!(card["list-text"], "Chrome on Android: Camera 1\nSafari on iPhone: Camera 2");
    assert_eq!(card["show-list"], true);
}

#[test]
fn a_stopped_browser_waits_until_it_connects_again() {
    let mut r = Rig::new();
    r.select(SOURCE_ID);
    let (feed, gen) = r.arrive("pixel-aaaaaaaa", "");
    r.host.stop_stream("browser:pixel-aaaaaaaa"); // the x on its tile
    r.changed();
    assert_eq!(r.host.starts, ["browser:pixel-aaaaaaaa"]);
    assert_eq!(r.launcher().redirect().as_deref(), Some("browser:pixel-aaaaaaaa")); // Start streams it again

    feed.disconnect(gen);
    r.plugin.on_server_changed(&mut r.host);
    assert!(!r.host.sources.contains_key("browser:pixel-aaaaaaaa")); // gone from the picker
    r.arrive("pixel-aaaaaaaa", "");
    assert_eq!(r.host.starts, ["browser:pixel-aaaaaaaa", "browser:pixel-aaaaaaaa"]);
}

#[test]
fn a_streaming_browser_that_drops_stays_until_it_comes_back() {
    let mut r = Rig::new();
    r.select(SOURCE_ID);
    let (feed, gen) = r.arrive("pixel-aaaaaaaa", "");
    feed.disconnect(gen);
    r.plugin.on_server_changed(&mut r.host);
    assert!(r.host.sources.contains_key("browser:pixel-aaaaaaaa"));
    assert!(Arc::ptr_eq(&r.plugin.hub.feed("pixel-aaaaaaaa").unwrap(), &feed));
    r.arrive("pixel-aaaaaaaa", "");
    assert_eq!(r.host.starts, ["browser:pixel-aaaaaaaa"]); // still streaming: nothing to start
}

#[test]
fn a_new_link_ends_the_streams_it_cuts_off_without_starting_another() {
    let mut r = Rig::new();
    r.select(SOURCE_ID);
    r.arrive("pixel-aaaaaaaa", "Chrome on Linux");
    r.changed();
    r.arrive("iphone-bbbbbbbb", "Safari on iPhone");
    r.host.stop_stream("browser:iphone-bbbbbbbb"); // connected, not streaming
    r.changed();
    assert_eq!(r.host.starts, ["browser:pixel-aaaaaaaa", "browser:iphone-bbbbbbbb"]);

    r.plugin.new_link(&mut r.host);
    assert!(r.host.streams.is_empty());
    assert_eq!(r.host.starts, ["browser:pixel-aaaaaaaa", "browser:iphone-bbbbbbbb"]);
    assert!(r.host.stopped.contains(&"browser:pixel-aaaaaaaa".to_string()) && r.plugin.connected().is_empty());
    assert!(r.browser_sources().is_empty()); // gone from the picker
    assert!(r.plugin.hub.feeds().is_empty() && *r.server().token.lock().unwrap() == "t2");
}

#[test]
fn two_browsers_of_one_kind_are_told_apart_in_the_picker() {
    let mut r = Rig::new();
    r.select(SOURCE_ID);
    let (first, gen) = r.arrive("linux-aaaaaaaa", "Chrome on Linux");
    r.changed();
    r.arrive("linux-bbbbbbbb", "Chrome on Linux");
    r.changed();
    assert_eq!(r.name_of("browser:linux-aaaaaaaa"), "Chrome on Linux");
    assert_eq!(r.name_of("browser:linux-bbbbbbbb"), "Chrome on Linux (2)");
    let known = r.plugin.known.iter().find(|(id, _)| id == "linux-bbbbbbbb").unwrap();
    assert_eq!(known.1.name, "Chrome on Linux"); // remembered by what it is

    first.disconnect(gen); // reconnecting: it keeps its name, and the other its number
    r.plugin.on_server_changed(&mut r.host);
    assert_eq!(r.name_of("browser:linux-aaaaaaaa"), "Chrome on Linux");
    assert_eq!(r.name_of("browser:linux-bbbbbbbb"), "Chrome on Linux (2)");
}

#[test]
fn no_room_means_no_feed() {
    let mut r = Rig::new();
    r.select(SOURCE_ID);
    r.host.streams = vec!["phone-1".into(), "phone-2".into(), "phone-3".into()];
    r.changed();
    assert!(r.plugin.hub.join("pixel-aaaaaaaa").is_some()); // the last camera
    assert!(r.plugin.hub.join("iphone-bbbbbbbb").is_none());
}

#[test]
fn the_server_stays_up_while_a_browser_streams_behind_another() {
    let mut r = Rig::new();
    r.select(SOURCE_ID);
    let server = r.server();
    r.arrive("pixel-aaaaaaaa", "");

    r.select("some-phone"); // the panels moved to a phone's tile
    assert!(!server.stopped.load(Ordering::SeqCst) && r.card()["shown"] == false);

    r.host.streams.clear();
    r.changed(); // its stream stopped
    assert!(server.stopped.load(Ordering::SeqCst) && !r.host.sources.contains_key("browser:pixel-aaaaaaaa"));
}

#[test]
fn adding_a_browser_next_to_a_phone_shows_the_code_until_one_joins() {
    let mut r = Rig::new();
    r.host.streams.push("some-phone".into());
    r.select(SOURCE_ID); // + put the panels on it: its own page, with the code
    assert!(!r.launcher().prepare(true));
    r.pump();
    let server = r.server();
    assert!(server.started.load(Ordering::SeqCst) && r.card()["shown"] == true && r.card()["qr-size"].as_u64() > Some(0));
    r.select("some-phone"); // the phone's tile clicked: its settings, without the code
    r.changed();
    assert!(!server.stopped.load(Ordering::SeqCst) && r.card()["shown"] == false); // still waiting

    assert!(r.launcher().waiting() && !r.host.issues.contains_key("start")); // its tile says so, not a banner

    r.arrive("pixel-aaaaaaaa", "");
    assert!(r.host.starts == ["browser:pixel-aaaaaaaa"] && r.card()["shown"] == false);
    assert!(!r.launcher().waiting() && r.host.stopped == [SOURCE_ID]); // the waiting tile went
}

#[test]
fn closing_the_waiting_tile_or_the_banner_hides_the_code() {
    let mut r = Rig::new();
    r.host.streams.push("some-phone".into());
    let launcher = r.launcher();
    launcher.prepare(true);
    r.pump();
    launcher.cancel_wait(); // the tile's X
    r.pump();
    assert!(r.card()["shown"] == false && r.server().stopped.load(Ordering::SeqCst) && !launcher.waiting());

    // The banner goes through the plugin as the window runs it, with nothing else streaming
    let (maker, servers) = fake_servers(|| ServerStats::new(8767, 8767, false, "Browser camera"));
    let mut plugin = BrowserCameraPlugin::new(maker, || vec!["192.168.1.20".into()]);
    let mut host = telescope_core::testing::FakeHost::new();
    plugin.setup(&mut host);
    let launcher = plugin.launcher();
    let wait = |plugin: &mut BrowserCameraPlugin, host: &mut telescope_core::testing::FakeHost| {
        launcher.prepare(true);
        for (token, data) in host.take_posted() {
            plugin.on_event(host, &Event::Posted { owner: NAME.into(), token, data });
        }
    };
    let shown = |host: &telescope_core::testing::FakeHost| {
        let last = host.calls.iter().rfind(|c| c.starts_with("set_ui_props card ") && c.contains("\"shown\"")).unwrap();
        last.contains("\"shown\":true")
    };
    let last_stopped = |servers: &Servers| servers.lock().unwrap().last().unwrap().0.stopped.load(Ordering::SeqCst);

    wait(&mut plugin, &mut host);
    assert!(!last_stopped(&servers));
    let cancel = host.issues.iter().find(|(k, _)| k == "start").unwrap().1.actions[0].clone();
    assert_eq!(cancel.label, "Cancel");
    let IssueDoes::Plugin { id } = cancel.does else { panic!("{:?}", cancel.does) };
    plugin.on_issue_action(&mut host, "start", &id); // Cancel
    assert!(!shown(&host) && last_stopped(&servers) && !host.issues.iter().any(|(k, _)| k == "start"));

    wait(&mut plugin, &mut host);
    assert!(host.issues.iter().find(|(k, _)| k == "start").unwrap().1.tell_dismiss);
    plugin.on_issue_action(&mut host, "start", "dismiss"); // the banner's X
    assert!(!shown(&host) && last_stopped(&servers));
}

#[test]
fn the_card_shows_the_browser_the_panels_show() {
    let mut r = Rig::new();
    r.select(SOURCE_ID);
    let (feed, gen) = r.arrive("pixel-aaaaaaaa", "");
    feed.note_hello(gen, "Firefox on Android", "Not allowed");
    feed.note_rtc(gen, "VP8", "");
    feed.note_sent(gen, Sent { width: Some(1280), height: Some(720), fps: Some(24), limit: "cpu".into() });
    r.plugin.on_server_changed(&mut r.host);
    r.select("browser:pixel-aaaaaaaa");
    let card = r.card();
    assert_eq!(card["shown"], true);
    assert_eq!(card["status-text"], "Firefox on Android, VP8, 1280×720 at 24 fps");
    assert_eq!(card["status-kind"], "status_warn");
    assert!(card["status-tip"].as_str().unwrap().contains("processor"));
    assert_eq!(card["mic-text"], "No microphone: Not allowed");

    feed.note_sent(gen, Sent { width: Some(1280), height: Some(720), fps: Some(30), limit: String::new() });
    r.plugin.on_server_changed(&mut r.host);
    assert!(r.card()["status-kind"] == "status_ok" && r.card()["status-tip"] == "");

    feed.note_rtc(gen, "", "This computer couldn't set up the video connection.");
    r.plugin.on_server_changed(&mut r.host);
    assert_eq!(r.card()["status-text"], "This computer couldn't set up the video connection.");
    assert_eq!(r.card()["status-kind"], "status_err");
}

#[test]
fn sending_text_says_codec_size_and_rate_once_known() {
    let sized = |w, h, fps| Sent { width: Some(w), height: Some(h), fps, limit: String::new() };
    assert_eq!(sending_text("", &Sent::default()), "");
    assert_eq!(sending_text("H264", &Sent::default()), ", H.264");
    assert_eq!(sending_text("H264", &sized(1920, 1080, None)), ", H.264, 1920×1080");
    assert_eq!(sending_text("XYZ", &sized(640, 480, Some(15))), ", 640×480 at 15 fps");
}

#[test]
fn capture_settings_go_to_every_page_and_stream() {
    let mut r = Rig::new();
    r.select(SOURCE_ID);
    let (feed, _) = r.arrive("pixel-aaaaaaaa", "");
    r.arrive("iphone-bbbbbbbb", "");
    r.select("browser:pixel-aaaaaaaa");
    r.plugin.ui(&mut r.host, "fps-picked", &[json!(0)]);
    r.plugin.ui(&mut r.host, "size-picked", &[json!(2)]);
    assert_eq!(r.host.outputs, [("browser:pixel-aaaaaaaa".to_string(), 15), ("browser:iphone-bbbbbbbb".to_string(), 15)]);
    let cfg = feed.page_config().1;
    assert_eq!((cfg["width"].clone(), cfg["height"].clone(), cfg["fps"].clone()), (json!(1920), json!(1080), json!(15)));
    let (later, _) = r.arrive("iphone-bbbbbbbb", "");
    assert_eq!(later.page_config().1["fps"], 15);
    let cfg = r.plugin.get_config();
    assert_eq!((cfg["size"].clone(), cfg["fps"].clone(), cfg["address"].clone()), (json!("1080p"), json!(15), json!("192.168.1.20")));
}

#[test]
fn config_falls_back_on_bad_values() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"size": "8k", "fps": 120, "address": 5, "known": {"x": {"name": 3}}})).unwrap();
    assert_eq!(r.plugin.get_config(), json!({"size": "720p", "fps": 30, "address": "", "known": {}}));
}

#[test]
fn a_browser_is_remembered_once_it_has_settings_and_can_be_forgotten() {
    let mut r = Rig::new();
    let lists = |host: &TestHost| -> Vec<Vec<(String, String)>> {
        host.events.iter().filter_map(|e| match e {
            Event::RememberedSources { sources } => Some(sources.iter().map(|s| (s.id.clone(), s.name.clone())).collect()),
            _ => None,
        }).collect()
    };
    r.select(SOURCE_ID);
    r.arrive("pixel-aaaaaaaa", "Chrome on Android");
    r.arrive("guest-cccccccc", "Safari on iPhone");
    r.host.settings.push("browser:pixel-aaaaaaaa".into()); // zoomed in on it; the guest changed nothing
    r.changed();
    assert_eq!(lists(&r.host).last().unwrap(), &[("browser:pixel-aaaaaaaa".to_string(), "Chrome on Android".to_string())]);

    let (maker, _) = fake_servers(|| ServerStats::new(8767, 8767, false, "Browser camera"));
    let mut again = BrowserCameraPlugin::new(maker, Vec::new);
    again.setup_on(&mut r.host, None);
    again.set_config(&r.plugin.get_config()).unwrap();
    again.tidy_known(&mut r.host);
    assert_eq!(again.known.iter().map(|(id, _)| id.as_str()).collect::<Vec<_>>(), ["pixel-aaaaaaaa"]); // the guest left nothing

    again.known[0].1.seen = unix_now() - (REMEMBER_DAYS + 1.0) * 86400.0;
    again.tidy_known(&mut r.host);
    assert!(again.known.is_empty() && r.host.forgotten == ["browser:pixel-aaaaaaaa"]);

    r.host.settings.push("browser:pixel-aaaaaaaa".into());
    r.plugin.hear(&mut r.host, &Event::ForgetSourceRequested { source_id: "browser:pixel-aaaaaaaa".into() });
    assert!(!r.host.streams.contains(&"browser:pixel-aaaaaaaa".to_string()) && lists(&r.host).last().unwrap().is_empty());
    assert_eq!(r.host.forgotten, ["browser:pixel-aaaaaaaa", "browser:pixel-aaaaaaaa"]);
}

#[test]
fn a_server_that_cant_be_made_says_why_and_start_fails() {
    let mut r = Rig::new();
    r.plugin.make_server = Box::new(|_, _| Err("no certificate".into()));
    r.select(SOURCE_ID);
    assert!(r.card()["status-text"].as_str().unwrap().contains("no certificate"));
    assert!(!r.plugin.prepare(&mut r.host));
    assert!(r.host.issues["start"].text.contains("no certificate"));
}

#[test]
fn a_server_that_wont_start_says_why_and_start_fails() {
    struct Refuses;
    impl Serving for Refuses {
        fn start(&self) -> Result<(), String> {
            Err("port in use".into())
        }
        fn stop(&self) {}
        fn url_for(&self, _host: &str) -> String {
            String::new()
        }
        fn new_token(&self) {}
        fn stats(&self) -> Option<Arc<ServerStats>> {
            None
        }
    }
    let mut r = Rig::new();
    r.plugin.make_server = Box::new(|_, _| Ok(Box::new(Refuses) as Box<dyn Serving>));
    r.select(SOURCE_ID);
    assert!(r.card()["status-text"].as_str().unwrap().contains("port in use"));
    assert!(!r.plugin.prepare(&mut r.host));
    assert!(r.host.issues["start"].text.contains("port in use"));
}

#[test]
fn diagnostics_say_how_far_browsers_got_but_never_name_the_device() {
    let mut r = Rig::new();
    r.select(SOURCE_ID);
    let stats = r.stats();
    stats.note(Step::Connections);
    stats.note(Step::TlsFailed);
    let (feed, gen) = r.arrive("pixel-aaaaaaaa", "");
    feed.note_hello(gen, "Safari on iPhone", "");
    feed.note_rtc(gen, "H264", "");
    feed.note_sent(gen, Sent { width: Some(1280), height: Some(720), fps: Some(30), limit: "bandwidth".into() });
    let diag: HashMap<String, String> = r.plugin.diagnostics().into_iter().collect();
    assert_eq!(diag["Browser camera"], "1 browser connected, sending H.264, 1280×720 at 30 fps (held back: bandwidth)");
    assert_eq!(diag["Browser camera port"], "8767");
    assert!(diag["Browser camera reached"].starts_with("1 connections, 1 certificate refusals, 0 page loads"));
    assert!(!format!("{diag:?}").contains("iPhone"));

    let mut r = Rig::with(|| ServerStats::new(41234, 8767, true, "Browser camera"));
    r.select(SOURCE_ID);
    let diag: HashMap<String, String> = r.plugin.diagnostics().into_iter().collect();
    assert_eq!(diag["Browser camera port"], "41234 (8767 was taken)");
}

#[test]
fn waiting_hint_points_at_the_step_that_failed() {
    let stats = ServerStats::new(8767, 8767, false, "Browser camera");
    let t0 = stats.started;
    let late = t0 + HINT_AFTER + Duration::from_secs(1);
    assert_eq!(waiting_hint(Some(&stats), false, t0 + Duration::from_secs(5)), ""); // still scanning, probably
    assert!(waiting_hint(Some(&stats), false, late).contains("firewall allows port 8767"));
    assert_eq!(waiting_hint(Some(&stats), true, late), "");
    stats.note(Step::Connections);
    assert_eq!(waiting_hint(Some(&stats), false, late), "");
    stats.note(Step::TlsFailed);
    assert!(waiting_hint(Some(&stats), false, t0 + Duration::from_secs(1)).contains("certificate"));
    stats.note(Step::Pages);
    assert_eq!(waiting_hint(Some(&stats), false, late), ""); // on the page now
}

#[test]
fn card_shows_the_firewall_hint_once_the_wait_is_long() {
    let mut r = Rig::new();
    r.select(SOURCE_ID);
    assert_eq!(r.host.card["hint-text"], "");
    assert_eq!(r.host.timers, [TIMER_HINT]);
    let mut stats = ServerStats::new(8767, 8767, false, "Browser camera");
    stats.started -= HINT_AFTER;
    *r.server().aged.lock().unwrap() = Some(Arc::new(stats));
    r.plugin.hear(&mut r.host, &Event::TimerFired { owner: NAME.into(), token: TIMER_HINT.into() }); // what the timer does
    assert!(r.host.card["hint-text"].as_str().unwrap().contains("port 8767"), "{}", r.host.card["hint-text"]);
}

#[test]
fn feed_drops_frames_from_a_replaced_browser() {
    let feed = BrowserFeed::new();
    let old = feed.connect();
    let new = feed.connect();
    feed.put_video(old, Codec::H264, b"old".to_vec(), 0);
    assert!(feed.next_frame(new, Duration::from_millis(10)).is_none());
    feed.put_video(new, Codec::H264, b"new".to_vec(), 0);
    let frame = feed.next_frame(new, Duration::from_millis(10)).unwrap();
    assert_eq!((frame.seq, frame.data), (1, b"new".to_vec()));
}

#[test]
fn falling_behind_points_at_its_own_resolution_and_frame_rate() {
    let mut r = Rig::new();
    let behind = |r: &mut Rig, on: bool| r.plugin.hear(&mut r.host, &Event::StreamBehind { source_id: SOURCE_ID.into(), behind: on });
    behind(&mut r, true);
    assert!(!r.host.issues.contains_key("behind")); // a phone is picked: Stream output says what helps
    r.select(SOURCE_ID);
    behind(&mut r, true);
    let issue = &r.host.issues["behind"];
    assert!(issue.title == "Can't keep up" && issue.text.contains("Browser camera card") && issue.kind == "warn");
    r.plugin.set_capture(&mut r.host, Some("480p"), Some(15));
    behind(&mut r, true);
    assert_eq!(r.host.issues["behind"].text, "The device's browser can't send any faster.");
    behind(&mut r, false);
    assert!(!r.host.issues.contains_key("behind"));
}

#[test]
fn a_browser_stopped_here_leaves_room_for_another() {
    let mut r = Rig::new();
    r.select(SOURCE_ID);
    for bid in ["browser-aaaaaaaa", "browser-bbbbbbbb", "browser-cccccccc", "browser-dddddddd"] {
        r.arrive(bid, "");
    }
    r.changed();
    assert_eq!(r.host.stream_count(), 4);
    r.host.stop_stream("browser:browser-aaaaaaaa");
    r.changed();
    assert_eq!(r.host.stream_count(), 3);
    assert!(r.plugin.hub.join("browser-replacement").is_some());
}

#[test]
fn browsers_let_in_during_a_start_dont_outnumber_the_cameras() {
    let mut r = Rig::new();
    r.host.streams = vec!["phone-a".into(), "phone-b".into()];
    r.host.starting = true; // a third phone waking takes the third camera
    r.select(SOURCE_ID);
    r.arrive("browser-aaaaaaaa", ""); // the last camera
    assert!(r.plugin.hub.join("browser-bbbbbbbb").is_none());
}

#[test]
fn the_microphone_plugin_reaches_a_browsers_mic_and_hears_why_there_is_none() {
    let mut r = Rig::new();
    r.select(SOURCE_ID);
    let mics = r.plugin.mics();
    let (feed, gen) = r.arrive("pixel-aaaaaaaa", "Chrome on Android");
    assert!(mics.has("browser:pixel-aaaaaaaa") && !mics.has("some-phone"));
    assert!(mics.open("some-phone").is_none());
    assert_eq!(mics.key("browser:pixel-aaaaaaaa"), "browser:/audio");

    let mut mic = mics.open("browser:pixel-aaaaaaaa").unwrap().unwrap();
    assert_eq!(feed.page_config().1["audio"], true); // the page starts sending
    feed.put_audio(gen, &[1, 0, 2, 0]);
    assert_eq!(mic.read().unwrap().data, [1, 0, 2, 0]);
    drop(mic);
    assert_eq!(feed.page_config().1["audio"], false);

    feed.note_hello(gen, "Chrome on Android", "Not allowed");
    assert_eq!(mics.open("browser:pixel-aaaaaaaa").unwrap().err().as_deref(), Some("Not allowed"));
    assert!(mics.open("browser:left-long-ago").unwrap().is_err());
}

#[test]
fn the_card_compiles_and_has_every_property_the_plugin_sets() {
    let source = include_str!("../../ui/browser_camera.slint");
    let mut compiler = slint_interpreter::Compiler::default();
    compiler.set_library_paths([(telescope_ui_kit::LIBRARY.to_string(), telescope_ui_kit::library_path())].into_iter().collect());
    let result = spin(compiler.build_from_source(source.to_string(), "browser_camera.slint".into()));
    let errors: Vec<String> = result.diagnostics().filter(|d| d.level() == slint_interpreter::DiagnosticLevel::Error)
        .map(|d| d.to_string()).collect();
    assert!(errors.is_empty(), "{errors:?}");
    let def = result.component("BrowserCameraCard").unwrap();
    let names: Vec<String> = def.properties().map(|(n, _)| n.replace('_', "-")).collect();
    let r = Rig::new();
    for key in r.card().as_object().unwrap().keys() {
        assert!(names.contains(key), "browser_camera.slint has no {key}");
    }
}

fn spin<F: std::future::Future>(f: F) -> F::Output {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    let mut f = std::pin::pin!(f);
    loop {
        if let std::task::Poll::Ready(v) = f.as_mut().poll(&mut cx) {
            return v;
        }
    }
}
