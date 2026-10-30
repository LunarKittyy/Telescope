use super::*;
use std::sync::{Arc, Mutex};
use telescope_core::host::BackgroundStream;
use telescope_core::testing::FakeHost;

struct Rig {
    plugin: NotesPlugin,
    host: FakeHost,
    clock: Arc<Mutex<f64>>,
    /// How many calls were already looked at by sent()
    seen: usize,
}

fn state(takes_notes: bool, notes: bool, shown: bool, facing: &str, more: Value) -> Value {
    let mut s = json!({"cameras": [{"id": "1", "current": true, "facing": facing}], "notes": notes, "notes_shown": shown});
    if takes_notes {
        s["takes_notes"] = json!(true);
    }
    for (k, v) in more.as_object().into_iter().flatten() {
        s[k] = v.clone();
    }
    s
}

fn default_state() -> Value {
    state(true, false, false, "front", json!({}))
}

fn stream(id: &str) -> StreamInfo {
    StreamInfo { source_id: id.into(), url: format!("https://{id}/v1/video"), session: 0 }
}

impl Rig {
    /// A card with a phone paired and empty settings loaded, as the window does at startup.
    fn new() -> Rig {
        let clock = Arc::new(Mutex::new(100.0));
        let c = clock.clone();
        let mut rig = Rig { plugin: NotesPlugin::with_clock(move || *c.lock().unwrap()), host: FakeHost::new(), clock, seen: 0 };
        rig.plugin.setup(&mut rig.host);
        rig.event(Event::PhonesChanged { count: 1 });
        rig.plugin.set_config(&json!({})).unwrap();
        rig
    }

    fn event(&mut self, e: Event) {
        self.plugin.on_event(&mut self.host, &e);
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
    }

    fn click(&mut self, callback: &str) {
        self.ui(callback, vec![]);
    }

    fn type_text(&mut self, text: &str) {
        self.ui("text-edited", vec![json!(text)]);
    }

    /// A phone streaming and telling its state, with the panels on it.
    fn streaming(&mut self, id: &str, state: Value) {
        self.host.streaming = true;
        self.host.focused = Some(id.into());
        self.plugin.on_stream_start(&mut self.host, &stream(id));
        self.plugin.on_phone_state(&mut self.host, &state);
    }

    fn state(&mut self, state: Value) {
        self.plugin.on_phone_state(&mut self.host, &state);
    }

    /// The bodies sent to the phone since the last call.
    fn sent(&mut self) -> Vec<Value> {
        let out = self.host.calls[self.seen..].iter().filter(|c| c.starts_with("phone_control ")).map(|c| serde_json::from_str(&c[c.find('{').unwrap()..]).unwrap()).collect();
        self.seen = self.host.calls.len();
        out
    }

    /// The newest value the card was given for a property.
    fn shown(&self, key: &str) -> Value {
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix("set_ui_props card ")).filter_map(|j| serde_json::from_str::<Value>(j).ok())
            .find_map(|v| v.get(key).cloned()).unwrap_or(Value::Null)
    }

    fn status(&self) -> String {
        self.shown("status").as_str().unwrap().to_string()
    }

    fn enabled(&self, key: &str) -> bool {
        self.shown(key) == json!(true)
    }

    fn saves(&self) -> usize {
        self.host.calls.iter().filter(|c| *c == "schedule_save").count()
    }

    fn run(&mut self, id: &str, params: Value) -> Option<String> {
        self.plugin.run_action(&mut self.host, id, &params)
    }
}

fn facts() -> StatusFacts {
    StatusFacts { streaming: true, phone: true, takes_notes: Some(true), front: Some(true), ..Default::default() }
}

#[test]
fn a_set_is_named_by_its_first_line() {
    assert_eq!(set_name("\n  Welcome everyone  \nSecond"), "Welcome everyone");
    assert_eq!(set_name("   \n"), "");
    let long = set_name(&"x".repeat(100));
    assert_eq!(long.chars().count(), NAME_CHARS);
    assert!(long.ends_with('\u{2026}'));
}

#[test]
fn the_front_camera_is_read_from_the_current_lens() {
    assert_eq!(front_facing(&state(true, false, false, "front", json!({}))), Some(true));
    assert_eq!(front_facing(&state(true, false, false, "back", json!({}))), Some(false));
    // an older phone leaves facing out
    assert_eq!(front_facing(&json!({"cameras": [{"current": true}]})), None);
    assert_eq!(front_facing(&json!({})), None);
}

#[test]
fn the_status_says_what_stands_in_the_way_first() {
    let with = |f: fn(&mut StatusFacts)| {
        let mut x = facts();
        f(&mut x);
        notes_status(&x)
    };
    assert_eq!(with(|f| f.phone = false).1, "dim");
    assert!(with(|f| f.takes_notes = Some(false)).0.contains("Update Telescope"));
    assert_eq!(with(|f| f.too_long = true).1, "status_err");
    assert!(with(|f| f.streaming = false).0.contains("Start streaming"));
    assert!(with(|f| f.front = Some(false)).0.contains("front camera"));
    assert!(with(|f| f.sent = true).0.contains("Open Telescope"));
    assert_eq!(with(|f| { f.sent = true; f.shown = true; }), ("The notes are on the phone's screen.".to_string(), "status_ok"));
    assert!(with(|f| { f.sent = true; f.shown = true; f.front = Some(false); }).0.contains("back camera"));
    assert!(with(|f| { f.sent = true; f.shown = true; f.edited = true; }).0.contains("Send again"));
    assert!(with(|f| { f.sent = true; f.closed = true; }).0.contains("closed on the phone"));
    assert!(with(|f| { f.sent = true; f.preview = true; }).0.contains("preview"));
}

#[test]
fn text_in_any_script_counts_its_utf8_bytes() {
    assert_eq!(body_size("Привет"), r#"{"action": "notes", "text": "Привет"}"#.len());
    // 18,000 bytes, not six times that
    assert!(body_size(&"नमस्ते".repeat(1000)) < NOTES_MAX_BYTES);
}

#[test]
fn nothing_can_be_sent_without_a_stream() {
    let mut r = Rig::new();
    r.type_text("Hello");
    assert!(!r.enabled("send-enabled"));
    assert!(r.status().contains("Start streaming"));
}

#[test]
fn send_puts_the_style_then_the_text_on_the_phone() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    r.type_text("Hello everyone\nThanks for coming");
    r.click("send-clicked");
    assert_eq!(r.sent(), vec![
        json!({"action": "notes_style", "size": 44, "speed": 3, "playing": 0}),
        json!({"action": "notes", "text": "Hello everyone\nThanks for coming"}),
    ]);
    assert!(r.enabled("nav-enabled") && r.enabled("clear-enabled"));
}

#[test]
fn scrolling_and_style_follow_while_the_notes_are_up() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    r.type_text("One\nTwo");
    r.click("send-clicked");
    r.sent();
    r.click("next-clicked");
    r.click("next-clicked");
    r.click("back-clicked");
    r.ui("play-toggled", vec![json!(true)]);
    r.ui("size-moved", vec![json!(60.0)]);
    r.ui("scroll-picked", vec![json!(1)]);
    assert_eq!(r.sent(), vec![
        json!({"action": "notes_scroll", "by": 1}), json!({"action": "notes_scroll", "by": 1}), json!({"action": "notes_scroll", "by": -1}),
        json!({"action": "notes_style", "size": 44, "speed": 3, "playing": 1}),
        json!({"action": "notes_style", "size": 60, "speed": 3, "playing": 1}),
        // by hand: Play goes and so does scrolling
        json!({"action": "notes_style", "size": 60, "speed": 0, "playing": 0}),
        json!({"action": "notes_style", "size": 60, "speed": 0, "playing": 0}),
    ]);
    assert_eq!(r.shown("show-auto"), json!(false));
}

#[test]
fn style_changes_before_sending_stay_on_the_desktop() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    r.ui("size-moved", vec![json!(80.0)]);
    r.click("next-clicked");
    assert!(r.sent().is_empty());
}

#[test]
fn clear_takes_the_notes_down() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    r.type_text("Hello");
    r.click("send-clicked");
    r.sent();
    r.click("clear-clicked");
    assert!(r.sent().contains(&json!({"action": "notes", "text": ""})));
    assert!(!r.enabled("clear-enabled") && !r.enabled("nav-enabled"));
}

#[test]
fn an_older_phone_is_asked_to_update_instead() {
    let mut r = Rig::new();
    r.streaming("p", state(false, false, false, "front", json!({})));
    r.type_text("Hello");
    assert!(!r.enabled("send-enabled"));
    assert!(r.status().contains("Update Telescope on the phone"));
    r.click("send-clicked");
    assert!(r.sent().is_empty());
}

#[test]
fn a_browser_camera_has_no_screen_for_notes() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    r.type_text("Hello");
    r.event(Event::SourceSelected { source_id: "browser:abc".into() });
    assert!(!r.enabled("send-enabled"));
    assert!(r.status().contains("phone as the camera"));
}

#[test]
fn notes_too_long_for_the_phone_are_held_back() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    // two bytes each in UTF-8
    r.type_text(&"é".repeat(NOTES_MAX_BYTES / 2 + 10));
    assert!(body_size(&r.plugin.sets[0]) > NOTES_MAX_BYTES);
    assert!(!r.enabled("send-enabled"));
    r.click("send-clicked");
    assert!(r.sent().is_empty());
}

#[test]
fn the_card_says_when_the_phone_has_them_but_not_on_screen() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    r.type_text("Hello");
    r.click("send-clicked");
    r.state(state(true, true, false, "front", json!({})));
    assert!(r.status().contains("Open Telescope on the phone"));
    r.state(state(true, true, true, "front", json!({})));
    assert_eq!(r.status(), "The notes are on the phone's screen.");
    r.type_text("Hello again");
    assert!(r.status().contains("Send again"));
}

#[test]
fn the_card_says_when_the_notes_were_closed_or_wait_for_preview() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    r.type_text("Hello");
    r.click("send-clicked");
    r.state(state(true, true, false, "front", json!({"notes_closed": true})));
    assert!(r.status().contains("closed on the phone"));
    r.state(state(true, true, false, "front", json!({"notes_preview": true})));
    assert!(r.status().contains("close its preview"));
    r.click("send-clicked");
    assert_eq!(r.status(), "The notes are on the phone's screen.");
}

#[test]
fn notes_the_phone_lost_are_sent_again() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    r.type_text("Hello");
    r.click("send-clicked");
    r.sent();
    r.state(state(true, false, false, "front", json!({})));
    // just sent: the phone may not have read it yet
    assert!(r.sent().is_empty());
    *r.clock.lock().unwrap() += 30.0;
    r.state(state(true, false, false, "front", json!({})));
    assert_eq!(r.sent().last(), Some(&json!({"action": "notes", "text": "Hello"})));
}

#[test]
fn a_stream_ending_forgets_what_was_sent() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    r.type_text("Hello");
    r.click("send-clicked");
    r.plugin.on_stream_stop(&mut r.host);
    r.sent();
    r.streaming("p", default_state());
    assert!(!r.enabled("clear-enabled"));
    r.click("next-clicked");
    assert!(r.sent().is_empty());
}

#[test]
fn moving_to_another_stream_and_back_sends_nothing_again() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    r.type_text("Hello");
    r.click("send-clicked");
    r.sent();
    r.plugin.on_focus_lost(&mut r.host);
    r.plugin.on_focus_gained(&mut r.host, &stream("p"));
    assert!(r.sent().is_empty());
    assert!(r.enabled("clear-enabled"));
}

#[test]
fn notes_on_one_phone_can_still_be_cleared_after_sending_to_another() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    r.type_text("For the first");
    r.click("send-clicked");
    r.plugin.on_focus_lost(&mut r.host);
    r.plugin.on_focus_gained(&mut r.host, &stream("other"));
    r.type_text("For the second");
    r.click("send-clicked");
    r.plugin.on_focus_lost(&mut r.host);
    r.plugin.on_focus_gained(&mut r.host, &stream("p"));
    assert!(r.enabled("clear-enabled") && r.enabled("nav-enabled"));
    r.sent();
    r.click("next-clicked");
    assert_eq!(r.sent().last(), Some(&json!({"action": "notes_scroll", "by": 1})));
    assert_eq!(r.host.calls.iter().rev().find(|c| c.starts_with("phone_control ")).unwrap().split(' ').nth(1).unwrap(), "Some(\"p\")");
    r.click("clear-clicked");
    assert_eq!(r.sent().first(), Some(&json!({"action": "notes", "text": ""})));
    r.plugin.on_focus_lost(&mut r.host);
    r.plugin.on_focus_gained(&mut r.host, &stream("other"));
    // the second still has its own
    assert!(r.enabled("clear-enabled"));
}

#[test]
fn another_phone_starting_leaves_the_first_ones_notes_in_reach() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    r.type_text("For the first");
    r.click("send-clicked");
    // a second phone starts and the panels move to it
    r.streaming("other", default_state());
    assert!(!r.enabled("clear-enabled"));
    r.plugin.on_focus_lost(&mut r.host);
    r.plugin.on_focus_gained(&mut r.host, &stream("p"));
    assert!(r.enabled("clear-enabled"));
}

#[test]
fn a_phone_stopped_behind_the_panels_is_forgotten_with_its_notes() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    r.type_text("For the first");
    r.click("send-clicked");
    // the panels move to a second phone, then the first stops
    r.streaming("other", default_state());
    r.event(Event::StreamsChanged { count: 1 });
    assert!(!r.plugin.sent.contains_key("p"));
    r.plugin.on_stream_stop(&mut r.host);
    r.host.streaming = false;
    r.event(Event::StreamsChanged { count: 0 });
    assert!(r.plugin.sent.is_empty());
}

#[test]
fn a_stream_still_running_behind_the_panels_keeps_its_notes() {
    let mut r = Rig::new();
    r.streaming("p", default_state());
    r.type_text("For the first");
    r.click("send-clicked");
    r.streaming("other", default_state());
    r.host.background = vec![BackgroundStream { source_id: "p".into(), name: "Phone".into() }];
    r.event(Event::StreamsChanged { count: 2 });
    assert!(r.plugin.sent.contains_key("p"));
}

#[test]
fn sets_are_saved_and_picked() {
    let mut r = Rig::new();
    r.type_text("Monday standup\n- wins");
    r.click("new-clicked");
    r.type_text("Interview");
    assert_eq!(r.shown("set-items"), json!(["Monday standup", "Interview"]));
    r.ui("set-picked", vec![json!(0)]);
    assert_eq!(r.shown("text"), json!("Monday standup\n- wins"));
    r.ui("size-moved", vec![json!(70.0)]);
    r.ui("scroll-picked", vec![json!(1)]);
    let cfg = r.plugin.get_config();
    assert_eq!(cfg, json!({"sets": ["Monday standup\n- wins", "Interview"], "current": 0, "size": 70, "speed": 3, "scroll": "hand"}));
    assert!(r.saves() > 0);

    r.plugin.set_config(&json!({})).unwrap();
    assert_eq!(r.plugin.get_config()["sets"], json!([""]));
    r.plugin.set_config(&cfg).unwrap();
    assert_eq!(r.plugin.get_config(), cfg);
    r.click("delete-clicked");
    assert_eq!(r.plugin.get_config()["sets"], json!(["Interview"]));
}

#[test]
fn the_card_hears_the_settings_that_were_loaded() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"sets": ["Hi", "Bye"], "current": 1, "size": 70})).unwrap();
    r.event(Event::PhonesChanged { count: 1 });
    assert_eq!(r.shown("text"), json!("Bye"));
    assert_eq!(r.shown("set-current"), json!(1));
    assert_eq!(r.shown("size-text"), json!("70"));
}

#[test]
fn nine_sets_are_the_most_and_the_last_one_cannot_be_deleted_away() {
    let mut r = Rig::new();
    for _ in 0..12 {
        r.click("new-clicked");
    }
    assert_eq!(r.plugin.sets.len(), MAX_SETS);
    assert!(!r.enabled("new-enabled"));
    for _ in 0..12 {
        r.click("delete-clicked");
    }
    assert_eq!(r.plugin.sets, vec![String::new()]);
    assert!(!r.enabled("delete-enabled"));
}

#[test]
fn a_damaged_config_falls_back_to_defaults() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"sets": ["a", 5, null], "current": 9, "size": "huge", "speed": 99, "scroll": "sideways"})).unwrap();
    assert_eq!(r.plugin.get_config(), json!({"sets": ["a"], "current": 0, "size": 44, "speed": 10, "scroll": "auto"}));
    r.plugin.set_config(&json!({"sets": "not a list"})).unwrap();
    assert_eq!(r.plugin.get_config()["sets"], json!([""]));
}

#[test]
fn shortcuts_can_scroll_and_send() {
    let mut p = NotesPlugin::new();
    let ids: Vec<String> = p.actions().into_iter().map(|a| a.id).collect();
    for id in ["notes.send", "notes.clear", "notes.next", "notes.previous", "notes.play", "notes.speed"] {
        assert!(ids.iter().any(|i| i == id), "{id}");
    }
}

#[test]
fn the_shortcuts_do_what_the_buttons_do_when_the_buttons_would_work() {
    let mut r = Rig::new();
    assert_eq!(r.run("notes.send", json!({})), None);
    r.streaming("p", default_state());
    r.type_text("One\nTwo");
    assert_eq!(r.run("notes.send", json!({})), Some("Send notes".into()));
    r.sent();
    assert_eq!(r.run("notes.next", json!({})), Some("Next line".into()));
    assert_eq!(r.run("notes.play", json!({"mode": "on"})), Some("Scroll notes: playing".into()));
    assert_eq!(r.sent().last(), Some(&json!({"action": "notes_style", "size": 44, "speed": 3, "playing": 1})));
    assert_eq!(r.run("notes.speed", json!({"op": "set", "value": 7})), Some("Notes scroll speed: 7".into()));
    assert_eq!(r.plugin.speed, 7);
    assert_eq!(r.run("notes.clear", json!({})), Some("Clear notes".into()));
    assert_eq!(r.run("notes.next", json!({})), None);
}

#[test]
fn switching_to_the_front_lens_clears_the_hint() {
    let mut r = Rig::new();
    r.streaming("p", state(true, false, false, "back", json!({})));
    r.type_text("Hello");
    assert!(r.status().contains("front camera"));
    r.event(Event::CameraSwitched { camera: json!({"id": "1", "facing": "front"}) });
    assert!(!r.status().contains("front camera"));
}

#[test]
fn it_is_an_optional_card_on_the_right_in_on_screen() {
    let info = NotesPlugin::new().info();
    assert_eq!((info.name.as_str(), info.category.as_str(), info.icon.as_str(), info.optional, info.in_presets, info.device_local), ("notes", "on_screen", "notes", true, false, false));
    assert_eq!(NotesPlugin::new().card().unwrap().region, "right");
}

#[test]
fn sent_notes_are_followed_by_asking_the_phone_its_state() {
    let mut r = Rig::new();
    r.streaming("phone", default_state());
    let asks = |r: &Rig| r.host.calls.iter().filter(|c| *c == "request_phone_state Some(\"phone\")").count();
    // Nothing sent yet and the phone said it takes notes: nothing to follow
    r.event(Event::TimerFired { owner: NAME.into(), token: T_POLL.into() });
    assert_eq!(asks(&r), 0);
    assert!(r.host.scheduled.iter().any(|(ms, t)| *ms == POLL_MS && t == T_POLL), "the poll keeps going");
    r.type_text("Say hello");
    r.click("send-clicked");
    assert!(r.host.scheduled.iter().any(|(ms, t)| *ms == FIRST_POLL_MS && t == T_FIRST_POLL));
    r.event(Event::TimerFired { owner: NAME.into(), token: T_FIRST_POLL.into() });
    r.event(Event::TimerFired { owner: NAME.into(), token: T_POLL.into() });
    assert_eq!(asks(&r), 2);
    // The answer comes back as the stream's state
    r.state(state(true, true, true, "front", json!({})));
    assert!(r.plugin.diagnostics().contains(&("Notes".into(), "on the phone".into())));
    r.plugin.on_stream_stop(&mut r.host);
    assert!(r.host.scheduled.is_empty(), "no polling once the stream stopped");
    r.event(Event::TimerFired { owner: NAME.into(), token: T_POLL.into() });
    assert_eq!(asks(&r), 2);
}
