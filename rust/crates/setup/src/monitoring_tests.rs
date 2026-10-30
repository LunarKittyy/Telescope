//! test_plugins.py's Monitoring tests, by the same names.

use super::*;
use telescope_core::host::BackgroundStream;
use telescope_core::testing::FakeHost;

struct Rig {
    plugin: MonitoringPlugin,
    host: FakeHost,
}

impl Rig {
    fn new() -> Rig {
        let mut host = FakeHost::new();
        host.focused = Some("phone-a".into());
        let mut plugin = MonitoringPlugin::new();
        plugin.on_event(&mut host, &Event::PhonesChanged { count: 1 });
        Rig { plugin, host }
    }

    fn start(&mut self, url: &str) {
        let stream = StreamInfo { source_id: "phone-a".into(), url: url.into(), session: 0 };
        self.plugin.on_stream_start(&mut self.host, &stream);
    }

    fn check(&mut self, level: i64, charging: bool, temp: f64) {
        self.plugin.check_alerts(&mut self.host, level, charging, temp);
    }

    fn event(&mut self, e: Event) {
        self.plugin.on_event(&mut self.host, &e);
    }

    /// Whether a stop was asked for since the stream started, as the Python host's is_streaming() turned false.
    fn stopped(&self) -> Vec<String> {
        self.host.calls.iter().filter(|c| c.starts_with("stop_stream")).cloned().collect()
    }

    fn saves(&self) -> usize {
        self.host.calls.iter().filter(|c| *c == "schedule_save").count()
    }

    fn streaming(&mut self) {
        self.host.streaming = true;
        self.host.calls.clear();
    }

    fn behind(&mut self, sid: &str, name: &str) {
        self.host.background = vec![BackgroundStream { source_id: sid.into(), name: name.into() }];
    }

    fn other(&mut self, sid: &str, state: Value) {
        self.event(Event::StreamState { source_id: sid.into(), state });
    }

    fn ui(&mut self, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: "card".into(), callback: callback.into(), args });
    }
}

#[test]
fn the_card_is_greyed_out_until_a_phone_is_paired() {
    let mut host = FakeHost::new();
    let mut plugin = MonitoringPlugin::new();
    assert_eq!(plugin.card().unwrap().ui.props["active"], json!(false));
    plugin.on_event(&mut host, &Event::PhonesChanged { count: 1 });
    assert_eq!(plugin.card().unwrap().ui.props["active"], json!(true));
    // A source that isn't a phone takes the card away
    plugin.on_event(&mut host, &Event::SourceSelected { source_id: "browser:1".into() });
    assert_eq!(plugin.card().unwrap().ui.props["phone-only-hidden"], json!(true));
}

#[test]
fn monitoring_threshold_changes_are_saved() {
    let mut r = Rig::new();
    let before = r.saves();
    r.ui("batt-committed", vec![json!("30")]);
    r.ui("temp-committed", vec![json!("50 °C")]);
    assert_eq!(r.saves(), before + 2);
    assert_eq!((r.plugin.battery_alert, r.plugin.temp_alert), (30, 50));
    r.ui("batt-step", vec![json!(1)]);
    r.ui("batt-committed", vec![json!("200")]);
    assert_eq!(r.plugin.battery_alert, 95, "held to the box's range");
}

#[test]
fn monitoring_stream_lifecycle() {
    let mut r = Rig::new();
    r.plugin.warned = Warned { batt: true, temp: true, last: None };
    r.start("url");
    assert!(r.plugin.polling && r.host.scheduled.iter().any(|(ms, t)| *ms == POLL_MS && t == POLL));
    assert!(!r.plugin.warned.batt && !r.plugin.warned.temp);

    r.plugin.on_stream_stop(&mut r.host);
    assert!(!r.plugin.polling && r.host.scheduled.is_empty());
    assert!(r.plugin.shown.is_none());
    assert_eq!(r.plugin.battery.value, "—");
    assert_eq!(r.plugin.temp.value, "—");
}

#[test]
fn monitoring_does_not_warn_again_when_the_same_stream_reconnects() {
    let mut r = Rig::new();
    r.start("url");
    r.check(10, false, 30.0);
    r.start("url");
    r.check(10, false, 30.0);
    assert_eq!(r.host.notifications.len(), 1);
}

#[test]
fn monitoring_does_not_warn_again_when_a_stream_hands_over_to_another_route() {
    let mut r = Rig::new();
    r.start("https://192.168.1.5:8080/v1/video.h264");
    r.check(15, false, 30.0);
    r.start("https://127.0.0.1:41000/v1/video.h264");
    assert_eq!(r.plugin.shown.as_deref(), Some("https://127.0.0.1:41000/v1/video.h264"));
    r.check(10, false, 30.0);
    assert_eq!(r.host.notifications.len(), 1);
}

#[test]
fn monitoring_keeps_what_it_warned_about_a_stream_that_handed_over_behind_the_panels() {
    let mut r = Rig::new();
    r.streaming();
    r.start("https://192.168.1.5:8080/v1/video.h264");
    r.check(15, false, 30.0);
    r.plugin.on_focus_lost(&mut r.host);
    r.plugin.on_focus_gained(&mut r.host, &StreamInfo { source_id: "phone-a".into(), url: "https://127.0.0.1:41000/v1/video.h264".into(), session: 0 });
    r.check(10, false, 30.0);
    assert_eq!(r.host.notifications.len(), 1);
}

#[test]
fn monitoring_warns_afresh_about_a_stream_that_started_again_behind_the_panels() {
    let mut r = Rig::new();
    r.streaming();
    r.start("url");
    r.check(10, false, 30.0);
    r.plugin.on_focus_lost(&mut r.host);
    // Stopped and started within one poll, so nothing behind the panels said it ended
    r.plugin.on_focus_gained(&mut r.host, &StreamInfo { source_id: "phone-a".into(), url: "url".into(), session: 1 });
    r.check(10, false, 30.0);
    assert_eq!(r.host.notifications.len(), 2);
}

#[test]
fn monitoring_warns_afresh_after_a_stop_and_a_new_start() {
    let mut r = Rig::new();
    r.start("url");
    r.check(15, false, 30.0);
    r.plugin.on_stream_stop(&mut r.host);
    r.start("url");
    r.check(10, false, 30.0);
    assert_eq!(r.host.notifications.len(), 2);
}

#[test]
fn monitoring_watches_a_stream_behind_the_panels_by_its_own_settings() {
    let mut r = Rig::new();
    r.streaming();
    r.behind("phone-b", "Pixel");
    r.host.device_configs.insert(("phone-b".into(), NAME.into()), json!({"battery_alert": 10, "temp_stop": true, "battery_stop": "yes"}));
    r.other("phone-b", json!({"battery": 15, "charging": false, "battery_temp_c": 30}));
    assert!(r.host.notifications.is_empty()); // 15% is fine by its own 10% threshold
    r.other("phone-b", json!({"battery": 50, "charging": true, "battery_temp_c": 50}));
    assert_eq!(r.host.notifications, vec![("Telescope - Phone Running Hot".to_string(),
                                           "Pixel: temperature is 50.0 °C. Stopped streaming to let it cool down.".to_string(), false)]);
    assert_eq!(r.stopped(), vec!["stop_stream Some(\"phone-b\")"]); // that one, not the one on screen

    r.host.background.clear(); // stopped meanwhile: a late reading is dropped
    r.other("phone-b", json!({"battery": 1, "charging": false, "battery_temp_c": 60}));
    assert_eq!(r.host.notifications.len(), 1);
}

#[test]
fn monitoring_doesnt_warn_again_after_a_tile_switch() {
    let mut r = Rig::new();
    r.streaming();
    r.start("url");
    r.check(10, false, 30.0);
    r.plugin.on_focus_lost(&mut r.host); // the panels moved to another stream
    assert!(r.plugin.polling); // still watching this one from behind
    r.plugin.on_focus_gained(&mut r.host, &StreamInfo { source_id: "phone-a".into(), url: "url".into(), session: 0 }); // and back
    r.check(10, false, 30.0);
    assert_eq!(r.host.notifications.len(), 1);
}

#[test]
fn monitoring_warns_afresh_about_a_stream_that_ended() {
    let mut r = Rig::new();
    r.streaming();
    r.start("url");
    r.check(10, false, 30.0);
    r.plugin.on_stream_stop(&mut r.host); // really stopped, with another stream carrying on
    assert!(r.plugin.polling);
    r.plugin.on_focus_gained(&mut r.host, &StreamInfo { source_id: "phone-a".into(), url: "url".into(), session: 0 });
    r.check(10, false, 30.0);
    assert_eq!(r.host.notifications.len(), 2);
}

#[test]
fn monitoring_skips_a_reading_it_cant_read() {
    let mut r = Rig::new();
    r.event(Event::PhoneStateUpdated { state: json!({"battery": "full", "battery_temp_c": null}) });
    assert_eq!(r.plugin.battery.value, "—");
}

#[test]
fn monitoring_ignores_state_without_battery() {
    let mut r = Rig::new();
    r.event(Event::PhoneStateUpdated { state: json!({"battery_temp_c": 99}) });
    assert_eq!(r.plugin.battery.value, "—");
}

#[test]
fn monitoring_display_colours() {
    for (level, charging, temp, batt, heat) in [
        (10, false, 50.0, "status_err", "status_err"),
        (25, false, 42.0, "status_warn", "status_warn"),
        (80, false, 30.0, "status_ok", "status_ok"),
        (10, true, 30.0, "status_ok", "status_ok"),
    ] {
        let mut r = Rig::new();
        r.plugin.update_display(&mut r.host, level, charging, temp);
        assert_eq!(r.plugin.battery.kind, batt);
        assert_eq!(r.plugin.temp.kind, heat);
        assert_eq!(r.plugin.battery.value, format!("{level}%"));
        assert_eq!(r.plugin.battery.note, if charging { "Charging" } else { "" });
        assert_eq!(r.plugin.temp.value, format!("{temp:.1} °C"));
    }
}

#[test]
fn monitoring_alerts_once_then_rearm_after_hysteresis() {
    let mut r = Rig::new();
    r.check(20, false, 45.0);
    r.check(10, false, 50.0);
    assert_eq!(r.host.notifications.len(), 2);
    assert!(r.host.notifications[0].0.contains("Low Battery"));
    assert!(r.host.notifications[1].0.contains("Running Hot"));

    r.check(26, false, 39.9);
    assert!(!r.plugin.warned.batt && !r.plugin.warned.temp);
    r.check(20, false, 45.0);
    assert_eq!(r.host.notifications.len(), 4);
}

#[test]
fn monitoring_charging_suppresses_battery_alert() {
    let mut r = Rig::new();
    r.check(5, true, 20.0);
    assert!(r.host.notifications.is_empty());
}

#[test]
fn monitoring_charging_but_falling_still_alerts() {
    // A wonky charger: charging stays true but the level keeps dropping
    let mut r = Rig::new();
    r.check(25, true, 20.0);
    assert!(r.host.notifications.is_empty());
    r.check(19, true, 20.0);
    assert_eq!(r.host.notifications.len(), 1);
    assert!(r.host.notifications[0].0.contains("Low Battery"));
    assert!(r.host.notifications[0].1.contains("plugged in"));
}

#[test]
fn monitoring_charging_rising_does_not_alert() {
    let mut r = Rig::new();
    for level in [15, 18, 22] {
        r.check(level, true, 20.0);
    }
    assert!(r.host.notifications.is_empty());
}

#[test]
fn monitoring_charging_plateau_does_not_alert() {
    // Flat readings while charging aren't falling
    let mut r = Rig::new();
    for _ in 0..3 {
        r.check(15, true, 20.0);
    }
    assert!(r.host.notifications.is_empty());
}

#[test]
fn monitoring_fetch_emits_only_valid_battery_state() {
    let mut r = Rig::new();
    r.event(Event::PhoneStateUpdated { state: json!({"cameras": []}) });
    r.event(Event::PhoneStateUpdated { state: json!(null) });
    assert_eq!(r.plugin.battery.value, "—");
    r.event(Event::PhoneStateUpdated { state: json!({"battery": 80}) });
    assert_eq!(r.plugin.battery.value, "80%");
    assert_eq!(reading(&json!({"battery": 80})), Some((80, true, 0.0)));
}

#[test]
fn monitoring_drops_a_reading_from_a_stream_that_ended() {
    // The window drops a state read before a restart that lands after it (apply_state's session check); what reaches here
    // as another stream's state is dropped once that stream isn't behind the panels any more
    let mut r = Rig::new();
    r.start("url");
    r.other("phone-old", json!({"battery": 3, "charging": false}));
    assert!(r.plugin.battery.value == "—" && r.host.notifications.is_empty());
    r.event(Event::PhoneStateUpdated { state: json!({"battery": 60, "charging": false}) });
    assert_eq!(r.plugin.battery.value, "60%");
}

#[test]
fn monitoring_stops_for_low_battery_only_when_asked() {
    let mut r = Rig::new();
    r.streaming();
    r.check(10, false, 30.0);
    assert!(r.stopped().is_empty() && r.host.notifications.len() == 1); // the alert, no stop
    r.plugin.set_switch(&mut r.host, "batt-stop", true);
    r.check(10, false, 30.0);
    assert_eq!(r.stopped(), vec!["stop_stream None"]);
    assert_eq!(r.host.notifications.last().unwrap().1, "Phone battery is at 10%. Stopped streaming.");
}

#[test]
fn monitoring_stops_for_heat_only_when_asked() {
    let mut r = Rig::new();
    r.streaming();
    r.plugin.set_switch(&mut r.host, "batt-stop", true); // battery is fine, so this stays out of it
    r.check(80, false, 50.0);
    assert!(r.stopped().is_empty());
    r.plugin.set_switch(&mut r.host, "temp-stop", true);
    r.check(80, false, 50.0);
    assert_eq!(r.stopped().len(), 1);
    assert!(r.host.notifications.last().unwrap().1.contains("Stopped streaming"));
}

#[test]
fn monitoring_stops_again_after_a_restart_past_the_limit() {
    let mut r = Rig::new();
    r.plugin.set_switch(&mut r.host, "temp-stop", true);
    for n in 0..2 {
        r.streaming();
        r.start(&format!("url-{n}"));
        r.check(80, false, 50.0);
        assert_eq!(r.stopped().len(), 1);
        r.host.streaming = false;
        r.plugin.on_stream_stop(&mut r.host);
    }
}

#[test]
fn monitoring_says_why_it_stopped_in_the_window_and_can_start_anyway() {
    let mut r = Rig::new();
    r.streaming();
    r.plugin.set_switch(&mut r.host, "batt-notify", false); // no notification: the banner still says it
    r.plugin.set_switch(&mut r.host, "batt-stop", true);
    r.start("url");
    r.check(15, false, 30.0);
    let (key, issue) = r.host.issues.last().unwrap().clone();
    assert_eq!(key, "stopped:phone-a");
    assert!(issue.title == "The phone's battery is at 15%" && issue.text.contains("20%"));
    assert_eq!(issue.actions.iter().map(|a| a.label.as_str()).collect::<Vec<_>>(), ["Start anyway"]);
    let IssueDoes::StartAgain { source_id, before } = &issue.actions[0].does else { panic!("starts again") };
    assert_eq!(source_id.as_deref(), Some("phone-a"));
    r.plugin.on_issue_action(&mut r.host, &key, before.as_deref().unwrap());

    r.streaming(); // the start went through: still low, but it keeps going
    r.host.streaming = false;
    r.plugin.on_stream_stop(&mut r.host);
    r.streaming();
    r.start("url-2");
    r.check(14, false, 30.0);
    assert!(r.stopped().is_empty());
    r.check(30, true, 30.0); // charged up past the limit: the next drop stops it again
    r.check(15, false, 30.0);
    assert_eq!(r.stopped().len(), 1);
}

#[test]
fn monitoring_start_anyway_for_heat_still_stops_for_battery() {
    let mut r = Rig::new();
    r.streaming();
    r.behind("phone-b", "Pixel");
    r.host.device_configs.insert(("phone-b".into(), NAME.into()), json!({"temp_stop": true, "battery_stop": true}));
    r.other("phone-b", json!({"battery": 80, "charging": false, "battery_temp_c": 50}));
    let (key, issue) = r.host.issues.last().unwrap().clone();
    assert!(issue.title == "Pixel is at 50.0 °C" && issue.text.contains("45 °C"));
    let IssueDoes::StartAgain { source_id, before } = &issue.actions[0].does else { panic!("starts again") };
    assert_eq!(source_id.as_deref(), Some("phone-b"));
    r.plugin.on_issue_action(&mut r.host, &key, before.as_deref().unwrap());
    r.streaming(); // streaming again
    r.other("phone-b", json!({"battery": 80, "charging": false, "battery_temp_c": 50}));
    assert!(r.stopped().is_empty());
    r.other("phone-b", json!({"battery": 10, "charging": false, "battery_temp_c": 50}));
    assert_eq!(r.stopped(), vec!["stop_stream Some(\"phone-b\")"]);
}

#[test]
fn monitoring_stops_quietly_with_notify_off() {
    let mut r = Rig::new();
    r.streaming();
    r.plugin.set_switch(&mut r.host, "batt-notify", false);
    r.plugin.set_switch(&mut r.host, "temp-notify", false);
    r.plugin.set_switch(&mut r.host, "batt-stop", true);
    r.check(10, false, 50.0);
    assert!(r.stopped().len() == 1 && r.host.notifications.is_empty());
}

#[test]
fn monitoring_charging_and_rising_never_stops() {
    let mut r = Rig::new();
    r.streaming();
    r.plugin.set_switch(&mut r.host, "batt-stop", true);
    r.check(10, true, 30.0);
    r.check(12, true, 30.0);
    assert!(r.stopped().is_empty());
    r.check(11, true, 30.0); // still dropping on the charger
    assert_eq!(r.stopped().len(), 1);
}

#[test]
fn monitoring_alert_settings_round_trip_and_defaults() {
    let mut r = Rig::new();
    assert_eq!(r.plugin.get_config(), json!({"battery_alert": 20, "temp_alert": 45, "battery_notify": true,
                                             "battery_stop": false, "temp_notify": true, "temp_stop": false}));
    let cfg = json!({"battery_alert": 30, "temp_alert": 50, "battery_notify": false, "battery_stop": true, "temp_notify": false, "temp_stop": true});
    r.plugin.set_config(&cfg).unwrap();
    assert_eq!(r.plugin.get_config(), cfg);
    r.plugin.set_config(&json!({"battery_alert": 30, "temp_alert": 50, "battery_stop": "yes", "temp_notify": 0})).unwrap();
    assert_eq!(r.plugin.get_config()["battery_stop"], json!(false));
    assert_eq!(r.plugin.get_config()["temp_notify"], json!(true));
}

#[test]
fn monitoring_poll_starts_daemon_fetch_thread() {
    // In place of a thread per stream: each phone stream is asked for its state, the shown one only while there is one
    let mut r = Rig::new();
    r.host.streaming = true;
    r.start("url");
    r.behind("phone-b", "Pixel");
    r.event(Event::TimerFired { owner: NAME.into(), token: POLL.into() });
    let asked: Vec<&String> = r.host.calls.iter().filter(|c| c.starts_with("request_phone_state")).collect();
    assert_eq!(asked, ["request_phone_state None", "request_phone_state Some(\"phone-b\")"]);
    assert!(r.plugin.polling, "polls again in 15 s");

    r.plugin.on_focus_lost(&mut r.host);
    r.host.background.clear();
    r.host.calls.clear();
    r.event(Event::TimerFired { owner: NAME.into(), token: POLL.into() });
    assert!(!r.host.calls.iter().any(|c| c.starts_with("request_phone_state")));
}

#[test]
fn monitoring_bus_subscription_and_config() {
    let mut r = Rig::new();
    r.plugin.set_config(&json!({"battery_alert": 30, "temp_alert": 50})).unwrap();
    assert_eq!((r.plugin.get_config()["battery_alert"].clone(), r.plugin.get_config()["temp_alert"].clone()), (json!(30), json!(50)));
    r.event(Event::PhoneStateUpdated { state: json!({"battery": 29, "charging": false, "battery_temp_c": 49}) });
    assert_eq!(r.plugin.battery.value, "29%");
    assert_eq!(r.plugin.temp.value, "49.0 °C");
}

#[test]
fn odd_saved_values_load_as_the_spin_boxes_take_them() {
    let mut p = MonitoringPlugin::new();
    p.set_config(&json!({"battery_alert": "  40 ", "temp_alert": 100})).unwrap();
    assert_eq!((p.battery_alert, p.temp_alert), (40, 65));
    p.set_config(&json!({"battery_alert": 0, "temp_alert": true})).unwrap();
    assert_eq!((p.battery_alert, p.temp_alert), (40, 35), "0 is skipped, True is 1");
    assert!(p.set_config(&json!({"battery_alert": "lots"})).is_err());
}
