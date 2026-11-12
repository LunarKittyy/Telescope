use super::*;
use std::sync::Mutex;
use telescope_core::host::HostCamera;
use telescope_core::testing::FakeHost;

#[derive(Default)]
struct FakeAutostart {
    on: Mutex<bool>,
    calls: Mutex<Vec<String>>,
    /// What turning it on or off answers with instead of working.
    refuse: Mutex<Option<String>>,
}

impl Autostart for FakeAutostart {
    fn enabled(&self) -> bool {
        *self.on.lock().unwrap()
    }
    fn set_enabled(&self, on: bool, minimized: bool) -> Result<(), String> {
        self.calls.lock().unwrap().push(format!("{} {minimized}", if on { "on" } else { "off" }));
        match self.refuse.lock().unwrap().clone() {
            Some(why) => Err(why),
            None => {
                *self.on.lock().unwrap() = on;
                Ok(())
            }
        }
    }
}

struct Rig {
    plugin: StartupPlugin,
    host: FakeHost,
    autostart: Arc<FakeAutostart>,
}

impl Rig {
    fn new() -> Rig {
        let autostart = Arc::new(FakeAutostart::default());
        // The phone mic is off unless a test says so: the idle stop then stops, as it always did
        Rig { plugin: StartupPlugin::new(autostart.clone()), host: FakeHost::new(), autostart }
    }

    /// Where the Python tests had a plugin watching for apps: start when one opens the camera, stop when none has for a while.
    fn watching() -> Rig {
        let mut r = Rig::new();
        r.plugin.set_start_on(&mut r.host, "watched");
        r.plugin.set_stop_idle(&mut r.host, "own");
        r.ready("p1", true);
        r
    }

    fn event(&mut self, e: Event) {
        self.plugin.on_event(&mut self.host, &e);
    }

    fn ready(&mut self, phone: &str, ready: bool) {
        self.event(Event::PhoneReady { phone_id: phone.into(), ready });
    }

    fn watched(&mut self, watched: bool) {
        self.event(Event::CameraWatched { watched });
    }

    fn start_on(&mut self, mode: &str) {
        self.plugin.set_start_on(&mut self.host, mode);
    }

    fn stop_idle(&mut self, mode: &str) {
        self.plugin.set_stop_idle(&mut self.host, mode);
    }

    /// The stream the host was asked for is up.
    fn stream_runs(&mut self) {
        self.host.starting = false;
        self.host.streaming = true;
        self.event(Event::StreamStarted { url: "url".into() });
    }

    fn stream_ends(&mut self) {
        self.host.streaming = false;
        self.event(Event::StreamStopped);
    }

    fn start_fails(&mut self) {
        self.host.starting = false;
        self.event(Event::StreamStartFailed);
    }

    /// The saved settings loaded, and the host told, as at startup.
    fn configure(&mut self, cfg: Value) {
        self.plugin.set_config(&cfg).unwrap();
        self.plugin.on_plugins_ready(&mut self.host);
    }

    fn starts(&self) -> Vec<bool> {
        self.host.calls.iter().filter_map(|c| c.strip_prefix("start_stream ")).map(|v| v == "true").collect()
    }

    fn stops(&self) -> usize {
        self.host.calls.iter().filter(|c| c.starts_with("stop_stream")).count()
    }

    fn camera_calls(&self) -> Vec<&str> {
        self.host.calls.iter().filter_map(|c| c.strip_prefix("set_camera_on ")).collect()
    }

    fn keep(&self) -> Option<bool> {
        self.host.calls.iter().rev().find_map(|c| c.strip_prefix("set_keep_in_tray ")).map(|v| v == "true")
    }

    fn wanted(&self) -> Vec<(bool, bool)> {
        self.host.events.iter().filter_map(|e| match e { Event::PhoneWanted { wanted, held } => Some((*wanted, *held)), _ => None }).collect()
    }

    fn counting(&self) -> bool {
        self.host.is_scheduled(IDLE_STOP)
    }

    /// The wait runs out.
    fn fire(&mut self) {
        assert!(self.counting(), "no wait is counting");
        self.host.scheduled.retain(|(_, t)| t != IDLE_STOP);
        self.event(Event::TimerFired { owner: NAME.into(), token: IDLE_STOP.into() });
    }

    fn wait_ms(&self) -> u64 {
        self.host.scheduled.iter().find(|(_, t)| t == IDLE_STOP).map(|(ms, _)| *ms).unwrap()
    }

    fn ui(&mut self, ui: &str, callback: &str, args: Vec<Value>) {
        self.plugin.on_ui(&mut self.host, &UiEvent { ui: ui.into(), callback: callback.into(), args });
    }

    /// The newest value the section was given for a property.
    fn shown(&self, ui: &str, prop: &str) -> Value {
        let head = format!("set_ui_props {ui} ");
        self.host.calls.iter().rev().filter_map(|c| c.strip_prefix(&head)).filter_map(|p| serde_json::from_str::<Value>(p).ok())
            .find_map(|p| p.get(prop).cloned()).unwrap_or(Value::Null)
    }
}

// ── Starting when the phone is ready ──────────────────────────────────

// test_startup.py::test_off_by_default_and_never_starts
#[test]
fn off_by_default_and_never_starts() {
    let mut r = Rig::new();
    r.ready("p1", true);
    assert!(r.starts().is_empty());
}

// test_startup.py::test_starts_once_per_arrival_without_asking
#[test]
fn starts_once_per_arrival_without_asking() {
    let mut r = Rig::new();
    r.start_on("ready");
    assert_eq!(r.keep(), Some(true));
    r.ready("p1", true);
    r.ready("p1", true);
    assert_eq!(r.starts(), [false]);
    r.start_fails();
    r.ready("p1", true); // a failed start isn't retried on every status check
    assert_eq!(r.starts(), [false]);
    r.ready("p1", false); // phone went away
    r.ready("p1", true); // and came back
    assert_eq!(r.starts(), [false, false]);
}

// test_startup.py::test_stop_sticks_until_the_phone_goes_away
#[test]
fn stop_sticks_until_the_phone_goes_away() {
    let mut r = Rig::new();
    r.start_on("ready");
    r.event(Event::StreamStopped);
    r.ready("p1", true);
    assert!(r.starts().is_empty());
    r.ready("p1", false);
    r.ready("p1", true);
    assert_eq!(r.starts(), [false]);
}

// test_startup.py::test_switching_phones_is_a_new_arrival
#[test]
fn switching_phones_is_a_new_arrival() {
    let mut r = Rig::new();
    r.start_on("ready");
    r.ready("p1", true);
    r.start_fails();
    r.ready("p2", true);
    assert_eq!(r.starts(), [false, false]);
}

// test_startup.py::test_not_while_a_start_is_waking_the_phone
#[test]
fn not_while_a_start_is_waking_the_phone() {
    let mut r = Rig::new();
    r.start_on("ready");
    r.host.starting = true; // the user pressed Start; claiming it would make their stream stop by itself
    r.ready("p1", true);
    assert!(r.starts().is_empty() && !r.plugin.starting_own);
}

// test_startup.py::test_not_while_streaming
#[test]
fn not_while_streaming() {
    let mut r = Rig::new();
    r.start_on("ready");
    r.host.streaming = true;
    r.ready("p1", true);
    assert!(r.starts().is_empty());
}

// test_startup.py::test_the_two_old_checkboxes_carry_over
#[test]
fn the_two_old_checkboxes_carry_over() {
    let cases = [
        (json!({}), "off", "own"),
        (json!({"auto_stream": true}), "ready", "off"),
        (json!({"watch_stream": true}), "watched", "own"),
        (json!({"auto_stream": true, "watch_stream": true}), "watched", "own"), // both on from before: watching wins
        (json!({"auto_stream": "yes", "watch_stream": 1}), "off", "own"),
    ];
    for (old, start_on, stop_idle) in cases {
        let mut r = Rig::new();
        r.configure(old.clone());
        assert_eq!((r.plugin.start_on, r.plugin.stop_idle, r.plugin.stop_delay_s, r.plugin.notify_stop), (start_on, stop_idle, 15, true), "{old}");
        assert_eq!(r.keep(), Some(start_on != "off"), "{old}");
    }
}

// test_startup.py::test_config_round_trip_and_bad_values
#[test]
fn config_round_trip_and_bad_values() {
    let mut r = Rig::new();
    let cfg = json!({"start_on": "watched", "stop_idle": "any", "stop_delay_s": 300, "notify_stop": false});
    r.configure(cfg.clone());
    assert_eq!(r.plugin.get_config(), cfg);
    assert_eq!(r.keep(), Some(true));
    r.configure(json!({"start_on": "sometimes", "stop_idle": null, "stop_delay_s": "5", "notify_stop": "no"}));
    assert_eq!(r.plugin.get_config(), json!({"start_on": "off", "stop_idle": "own", "stop_delay_s": 15, "notify_stop": true}));
    assert_eq!(r.keep(), Some(false));
    r.configure(json!({"stop_idle": "own", "stop_delay_s": 2, "auto_stream": true})); // new keys win over old ones
    assert_eq!((r.plugin.start_on, r.plugin.stop_idle, r.plugin.stop_delay_s), ("off", "own", 10));
    r.configure(json!({"start_on": "ready", "stop_delay_s": 99_999}));
    assert_eq!(r.plugin.stop_delay_s, 3600);
    r.configure(json!({"start_on": "ready", "stop_delay_s": true}));
    assert_eq!(r.plugin.stop_delay_s, 15);
    // A whole number written with a point isn't a whole number to Python, so it's the default too
    r.configure(json!({"start_on": "ready", "stop_delay_s": 30.0}));
    assert_eq!(r.plugin.stop_delay_s, 15);
}

// test_startup.py::test_diagnostics_say_what_is_on_and_who_owns_the_stream
#[test]
fn diagnostics_say_what_is_on_and_who_owns_the_stream() {
    let mut r = Rig::new();
    r.start_on("watched");
    r.stop_idle("own");
    r.ready("p1", true);
    r.watched(true);
    r.event(Event::StreamStarted { url: "u".into() });
    let lines: Vec<(&str, &str)> = vec![
        ("Start streaming", "When an app opens the camera"),
        ("Stop streaming", "When no app uses the camera, if it started the stream"),
        ("Wait before stopping", "15 s"),
        ("Tell me when it stops a stream", "on"),
        ("Stream started automatically", "yes"),
    ];
    assert_eq!(r.plugin.diagnostics(), lines.into_iter().map(|(a, b)| (a.to_string(), b.to_string())).collect::<Vec<_>>());
}

// test_startup.py::test_ready_starts_from_the_tray
#[test]
fn ready_starts_from_the_tray() {
    // Opened at sign-in straight to the tray, the window is never shown, and the phone arriving still starts
    let mut r = Rig::new();
    r.start_on("ready");
    r.event(Event::WindowShown { shown: false });
    r.ready("p1", true);
    assert_eq!(r.starts(), [false]);
}

// test_startup.py::test_ready_asks_for_the_phone_to_be_checked_with_the_window_hidden
#[test]
fn ready_asks_for_the_phone_to_be_checked_with_the_window_hidden() {
    let mut r = Rig::new();
    r.start_on("ready");
    r.start_on("watched");
    r.configure(json!({"start_on": "ready"}));
    assert_eq!(r.wanted(), [(true, false), (false, false), (true, false)]);
}

// test_startup.py::test_ready_says_when_only_the_phone_leaving_matters
#[test]
fn ready_says_when_only_the_phone_leaving_matters() {
    // Started (or stopped) for this arrival: checks only need to notice the phone going away, so they can be slow
    let mut r = Rig::new();
    r.start_on("ready");
    r.ready("p1", true);
    assert_eq!(r.starts(), [false]);
    r.stream_runs();
    r.stream_ends();
    r.ready("p1", true);
    assert_eq!(r.wanted(), [(true, false), (true, true)]);
    r.ready("p1", false);
    assert_eq!(r.wanted().last(), Some(&(true, false)));
}

// ── Streaming while an app uses the camera ────────────────────────────

// test_startup.py::test_with_the_window_hidden_an_app_waits_for_a_fresh_check_of_the_phone
#[test]
fn with_the_window_hidden_an_app_waits_for_a_fresh_check_of_the_phone() {
    // The phone isn't checked while the window is hidden, so the ready it said before is no reason to start
    let mut r = Rig::watching();
    r.event(Event::WindowShown { shown: false });
    r.watched(true);
    assert!(r.starts().is_empty());
    r.ready("p1", true);
    assert_eq!(r.starts(), [false]);
}

// test_startup.py::test_with_the_window_hidden_an_app_does_not_start_for_a_phone_that_left
#[test]
fn with_the_window_hidden_an_app_does_not_start_for_a_phone_that_left() {
    let mut r = Rig::watching();
    r.event(Event::WindowShown { shown: false });
    r.watched(true);
    r.ready("p1", false);
    assert!(r.starts().is_empty());
}

// test_startup.py::test_an_app_opening_the_camera_just_after_the_window_comes_back_waits_for_a_fresh_check
#[test]
fn an_app_opening_the_camera_just_after_the_window_comes_back_waits_for_a_fresh_check() {
    // Hidden, the phone wasn't checked: the ready from before it was hidden says nothing about now
    let mut r = Rig::watching();
    r.event(Event::WindowShown { shown: false });
    r.event(Event::WindowShown { shown: true });
    r.watched(true);
    assert!(r.starts().is_empty());
    r.ready("p1", true);
    assert_eq!(r.starts(), [false]);
}

// test_startup.py::test_watching_is_off_by_default
#[test]
fn watching_is_off_by_default() {
    let mut r = Rig::new();
    r.ready("p1", true);
    r.watched(true);
    assert!(r.starts().is_empty());
}

// test_startup.py::test_starts_when_an_app_reads_the_camera_and_stops_after_it_lets_go
#[test]
fn starts_when_an_app_reads_the_camera_and_stops_after_it_lets_go() {
    let mut r = Rig::watching();
    assert!(r.starts().is_empty());
    r.watched(true);
    assert_eq!(r.starts(), [false]);
    r.stream_runs();
    r.watched(false);
    assert!(r.counting() && r.stops() == 0); // not straight away
    r.fire();
    assert_eq!(r.stops(), 1);
}

// test_startup.py::test_an_app_coming_back_in_time_keeps_the_stream
#[test]
fn an_app_coming_back_in_time_keeps_the_stream() {
    let mut r = Rig::watching();
    r.watched(true);
    r.stream_runs();
    r.watched(false);
    r.watched(true);
    assert!(!r.counting());
    assert_eq!(r.starts(), [false]);
}

// test_startup.py::test_waits_for_the_phone_when_an_app_reads_first
#[test]
fn waits_for_the_phone_when_an_app_reads_first() {
    let mut r = Rig::new();
    r.start_on("watched");
    r.stop_idle("own");
    r.watched(true);
    assert!(r.starts().is_empty());
    r.ready("p1", true);
    r.ready("p1", true); // every idle check; a failed start isn't retried
    assert_eq!(r.starts(), [false]);
}

// test_startup.py::test_never_stops_a_stream_it_did_not_start
#[test]
fn never_stops_a_stream_it_did_not_start() {
    let mut r = Rig::watching();
    r.stream_runs(); // the user pressed Start
    r.watched(true);
    r.watched(false);
    assert!(!r.counting());
    assert!(r.starts().is_empty() && r.stops() == 0);
}

// test_startup.py::test_a_start_already_waking_is_not_claimed
#[test]
fn a_start_already_waking_is_not_claimed() {
    let mut r = Rig::watching();
    r.host.starting = true; // the user pressed Start and the phone is waking
    r.watched(true);
    r.host.starting = false;
    r.stream_runs();
    r.watched(false);
    assert!(r.starts().is_empty() && !r.counting());
}

// test_startup.py::test_stop_sticks_until_the_app_lets_go
#[test]
fn stop_sticks_until_the_app_lets_go() {
    let mut r = Rig::watching();
    r.watched(true);
    r.stream_runs();
    r.stream_ends(); // the user pressed Stop mid-call
    r.ready("p1", true);
    assert_eq!(r.starts(), [false]);
    r.watched(false);
    r.watched(true); // a new call
    assert_eq!(r.starts(), [false, false]);
}

// test_startup.py::test_a_reconnect_keeps_the_stream_its_own
#[test]
fn a_reconnect_keeps_the_stream_its_own() {
    let mut r = Rig::watching();
    r.watched(true);
    r.stream_runs();
    r.host.restarting = true; // a reconnect or camera resize
    r.stream_ends();
    r.host.restarting = false;
    r.stream_runs();
    r.watched(false);
    r.fire();
    assert_eq!(r.starts(), [false]);
    assert_eq!(r.stops(), 1);
}

// test_startup.py::test_an_app_leaving_while_the_phone_wakes_still_stops_it
#[test]
fn an_app_leaving_while_the_phone_wakes_still_stops_it() {
    let mut r = Rig::watching();
    r.watched(true);
    r.watched(false); // left before the stream got going
    assert!(r.counting());
    r.fire();
    assert_eq!(r.stops(), 1);
}

// test_startup.py::test_switching_it_off_hands_the_stream_to_the_user
#[test]
fn switching_it_off_hands_the_stream_to_the_user() {
    let mut r = Rig::watching();
    r.watched(true);
    r.stream_runs();
    r.start_on("off");
    r.watched(false);
    assert!(!r.counting() && r.stops() == 0);
}

// ── Stopping once nothing reads the camera ────────────────────────────

// test_startup.py::test_a_manual_stream_stops_only_under_any
#[test]
fn a_manual_stream_stops_only_under_any() {
    let mut r = Rig::new();
    r.watched(false);
    for (mode, stops) in [("off", 0), ("own", 0), ("any", 1)] {
        r.stop_idle(mode);
        r.stream_runs(); // the user pressed Start
        if stops > 0 {
            r.fire();
        } else {
            assert!(!r.counting());
        }
        assert_eq!(r.stops(), stops);
        r.stream_ends();
    }
    assert_eq!(r.host.notifications, [("Telescope stopped streaming".to_string(), "No app used the camera for 15 s.".to_string(), false)]);
}

// test_startup.py::test_waits_the_chosen_time
#[test]
fn waits_the_chosen_time() {
    let mut r = Rig::new();
    r.watched(false);
    r.configure(json!({"stop_idle": "any", "stop_delay_s": 300}));
    r.stream_runs();
    assert_eq!(r.wait_ms(), 300_000);
}

// test_startup.py::test_changing_the_wait_mid_count_restarts_it
#[test]
fn changing_the_wait_mid_count_restarts_it() {
    let mut r = Rig::new();
    r.watched(false);
    r.stop_idle("any");
    r.stream_runs();
    r.plugin.set_stop_delay(&mut r.host, 60);
    assert!(r.counting());
    assert_eq!(r.wait_ms(), 60_000);
    r.plugin.set_stop_delay(&mut r.host, 3); // below the floor
    assert_eq!(r.plugin.stop_delay_s, 10);
    assert_eq!(r.wait_ms(), 10_000);
}

// test_startup.py::test_no_notice_when_it_is_switched_off
#[test]
fn no_notice_when_it_is_switched_off() {
    let mut r = Rig::new();
    r.watched(false);
    r.stop_idle("any");
    r.plugin.set_notify_stop(&mut r.host, false);
    r.stream_runs();
    r.fire();
    assert!(r.stops() == 1 && r.host.notifications.is_empty());
}

// test_startup.py::test_never_stops_before_the_watch_has_reported
#[test]
fn never_stops_before_the_watch_has_reported() {
    let mut r = Rig::new();
    r.stop_idle("any");
    r.stream_runs(); // the watch can't open the camera, so it never says anything
    assert!(!r.counting());
    r.watched(false);
    assert!(r.counting());
}

// test_startup.py::test_an_app_opening_the_camera_cancels_the_wait
#[test]
fn an_app_opening_the_camera_cancels_the_wait() {
    let mut r = Rig::new();
    r.watched(false);
    r.stop_idle("any");
    r.stream_runs();
    r.watched(true);
    assert!(!r.counting());
}

// test_startup.py::test_the_wait_ending_checks_again_before_stopping
#[test]
fn the_wait_ending_checks_again_before_stopping() {
    let mut r = Rig::new();
    r.watched(false);
    r.stop_idle("any");
    r.stream_runs();
    r.plugin.watched = true; // an app arrived in the same moment the timer fired
    r.fire();
    assert_eq!(r.stops(), 0);
}

// test_startup.py::test_a_restart_never_stops_or_shortens
#[test]
fn a_restart_never_stops_or_shortens() {
    let mut r = Rig::new();
    r.watched(false);
    r.stop_idle("any");
    r.stream_runs();
    r.host.restarting = true;
    r.stream_ends();
    r.host.restarting = false;
    r.stream_runs();
    assert!(r.counting() && r.stops() == 0);
}

// test_startup.py::test_switching_the_stop_off_cancels_the_wait
#[test]
fn switching_the_stop_off_cancels_the_wait() {
    let mut r = Rig::new();
    r.watched(false);
    r.stop_idle("any");
    r.stream_runs();
    r.stop_idle("off");
    assert!(!r.counting());
}

// test_startup.py::test_a_failed_start_does_not_mark_the_next_manual_stream
#[test]
fn a_failed_start_does_not_mark_the_next_manual_stream() {
    let mut r = Rig::watching();
    r.watched(true);
    assert_eq!(r.starts(), [false]);
    r.start_fails(); // the phone couldn't be woken
    r.stream_runs(); // then the user pressed Start by hand
    r.watched(false);
    assert!(!r.counting() && r.stops() == 0);
}

// test_startup.py::test_phone_ready_start_with_idle_stop_stays_stopped
#[test]
fn phone_ready_start_with_idle_stop_stays_stopped() {
    let mut r = Rig::new();
    r.watched(false);
    r.start_on("ready");
    r.stop_idle("own");
    r.ready("p1", true);
    r.stream_runs();
    r.fire();
    r.stream_ends();
    r.ready("p1", true);
    assert_eq!((r.starts(), r.stops()), (vec![false], 1)); // no start/stop loop
    r.ready("p1", false);
    r.ready("p1", true);
    assert_eq!(r.starts(), [false, false]);
}

// test_startup.py::test_watch_start_without_idle_stop_keeps_streaming
#[test]
fn watch_start_without_idle_stop_keeps_streaming() {
    let mut r = Rig::watching();
    r.stop_idle("off");
    r.watched(true);
    r.stream_runs();
    r.watched(false);
    assert!(!r.counting() && r.stops() == 0);
}

// test_startup.py::test_an_idle_stop_lets_the_next_app_start_it_again
#[test]
fn an_idle_stop_lets_the_next_app_start_it_again() {
    let mut r = Rig::watching();
    r.watched(true);
    r.stream_runs();
    r.watched(false);
    r.fire();
    r.stream_ends();
    r.watched(true);
    assert_eq!(r.starts(), [false, false]);
}

// test_startup.py::test_once_a_second_camera_streamed_nothing_stops_by_itself_until_all_stop
#[test]
fn once_a_second_camera_streamed_nothing_stops_by_itself_until_all_stop() {
    let mut r = Rig::watching();
    r.watched(true);
    r.stream_runs();
    r.watched(false);
    assert!(r.counting());
    r.host.streams = Some(2);
    r.event(Event::StreamsChanged { count: 2 });
    assert!(!r.counting());
    r.host.streams = Some(1);
    r.event(Event::StreamsChanged { count: 1 }); // the one left may stream to an extra camera, which no watch sees
    assert!(!r.counting());
    r.stream_ends();
    assert!(!r.plugin.several); // one camera again after a full stop
}

// test_startup.py::test_an_outside_stop_while_an_app_reads_holds_the_watch_start
#[test]
fn an_outside_stop_while_an_app_reads_holds_the_watch_start() {
    let mut r = Rig::watching();
    r.watched(true);
    r.stream_runs();
    r.stream_ends(); // Stop pressed, or Monitoring stopped it for battery or heat
    r.ready("p1", true);
    assert_eq!(r.starts(), [false]);
}

// ── With the phone mic on, only the camera turns off ──────────────────

// test_startup.py::test_with_the_mic_on_the_idle_stop_turns_only_the_camera_off
#[test]
fn with_the_mic_on_the_idle_stop_turns_only_the_camera_off() {
    let mut r = Rig::watching();
    r.host.can_turn_off = Some((true, String::new()));
    r.watched(true);
    r.stream_runs();
    r.watched(false);
    r.fire();
    assert_eq!(r.stops(), 0);
    assert_eq!(r.camera_calls(), ["false true"]);
    assert!(!r.counting()); // nothing more to count down while the camera is off
    assert!(r.host.notifications.is_empty()); // the tray icon shows it, no notification
}

// test_startup.py::test_the_camera_comes_back_when_an_app_opens_it_again
#[test]
fn the_camera_comes_back_when_an_app_opens_it_again() {
    let mut r = Rig::watching();
    r.host.can_turn_off = Some((true, String::new()));
    r.watched(true);
    r.stream_runs();
    r.watched(false);
    r.fire();
    r.watched(true);
    assert_eq!(r.camera_calls(), ["false true", "true true"]);
    assert_eq!(r.starts(), [false]);
    // and it goes off again the next time the app lets go
    r.watched(false);
    assert!(r.counting());
}

// test_startup.py::test_a_camera_turned_off_by_hand_stays_off_when_an_app_opens_it
#[test]
fn a_camera_turned_off_by_hand_stays_off_when_an_app_opens_it() {
    let mut r = Rig::watching();
    r.host.can_turn_off = Some((true, String::new()));
    r.stream_runs();
    r.host.set_camera_on(false, false); // the user, not the rule
    r.watched(true);
    assert_eq!(r.camera_calls(), ["false false"]);
}

// ── The settings window ───────────────────────────────────────────────

// test_startup.py::test_sign_in_switch_opens_telescope_at_sign_in
#[test]
fn sign_in_switch_opens_telescope_at_sign_in() {
    let mut r = Rig::new();
    r.ui("sign-in", "shown", vec![]);
    assert_eq!(r.shown("sign-in", "open-checked"), json!(false));
    r.ui("sign-in", "open-toggled", vec![json!(true)]);
    assert!(r.autostart.enabled());
    *r.autostart.refuse.lock().unwrap() = Some("denied".into());
    r.ui("sign-in", "open-toggled", vec![json!(false)]);
    assert_eq!(*r.autostart.calls.lock().unwrap(), ["on true", "off true"], "minimised, as the sign-in entry always is");
    assert!(r.host.calls.iter().any(|c| c == "show_issue startup denied"));
    // What the computer says, not what was last clicked
    *r.autostart.on.lock().unwrap() = true;
    r.ui("sign-in", "shown", vec![]);
    assert_eq!(r.shown("sign-in", "open-checked"), json!(true));
    assert_eq!(r.autostart.calls.lock().unwrap().len(), 2);
    // A switch that worked clears the banner
    *r.autostart.refuse.lock().unwrap() = None;
    r.ui("sign-in", "open-toggled", vec![json!(true)]);
    assert!(r.host.calls.iter().any(|c| c == "clear_issue Some(\"startup\")"));
}

// test_startup.py::test_settings_sections_go_in_general_and_streaming
#[test]
fn settings_sections_go_in_general_and_streaming() {
    let mut r = Rig::new();
    let sections = r.plugin.settings_sections();
    assert_eq!(sections.iter().map(|s| s.tab.as_str()).collect::<Vec<_>>(), ["general", "streaming"]);
    assert_eq!(sections.iter().map(|s| s.ui.component.as_str()).collect::<Vec<_>>(), ["StartupSettings", "AutomaticSettings"]);
    // Each brings itself up to date when the settings window shows it (on_show)
    for (id, shows) in [("sign-in", "open-checked"), ("automatic", "start-current")] {
        assert!(sections.iter().any(|s| s.id == id), "{id}");
        r.host.calls.clear();
        r.ui(id, "shown", vec![]);
        assert!(r.host.calls.iter().any(|c| c.contains(&format!("set_ui_props {id} ")) && c.contains(shows)), "{id}: {:?}", r.host.calls);
    }
}

// test_startup.py::test_start_and_stop_choices_reflect_and_change_the_settings
#[test]
fn start_and_stop_choices_reflect_and_change_the_settings() {
    let mut r = Rig::new();
    r.ui("automatic", "shown", vec![]);
    let (start, stop) = (r.shown("automatic", "start-items"), r.shown("automatic", "stop-items"));
    assert_eq!(start[r.shown("automatic", "start-current").as_u64().unwrap() as usize], "Only when I press Start");
    assert_eq!(stop[r.shown("automatic", "stop-current").as_u64().unwrap() as usize], "When no app uses the camera, if it started the stream");
    assert_eq!(r.shown("automatic", "stops"), json!(true));

    r.ui("automatic", "start-picked", vec![json!(2)]);
    assert_eq!((r.plugin.start_on, r.keep()), ("watched", Some(true)));
    r.ui("automatic", "start-picked", vec![json!(1)]);
    assert_eq!(r.plugin.start_on, "ready");
    r.ui("automatic", "stop-picked", vec![json!(2)]);
    assert_eq!(r.plugin.stop_idle, "any");
    // The wait and the notice come on with it, without the window being opened again
    assert_eq!(r.shown("automatic", "stops"), json!(true));

    // Changed behind its back (a preset, a reload), and shown again
    r.plugin.set_config(&json!({"start_on": "watched", "stop_idle": "off"})).unwrap();
    r.ui("automatic", "shown", vec![]);
    assert_eq!(r.shown("automatic", "start-current"), json!(2));
    assert_eq!(r.shown("automatic", "stops"), json!(false));

    r.ui("automatic", "notify-toggled", vec![json!(false)]);
    assert!(!r.plugin.notify_stop);
    r.ui("automatic", "shown", vec![]);
    assert_eq!(r.shown("automatic", "notify-checked"), json!(false));
}

// test_startup.py::test_wait_fields_read_and_write_the_wait
#[test]
fn wait_fields_read_and_write_the_wait() {
    let mut r = Rig::new();
    r.configure(json!({"stop_idle": "any", "stop_delay_s": 120}));
    r.ui("automatic", "shown", vec![]);
    assert_eq!((r.shown("automatic", "unit-current"), r.shown("automatic", "delay-text")), (json!(1), json!("2")));
    assert_eq!(r.shown("automatic", "unit-items"), json!(["seconds", "minutes"]));
    r.ui("automatic", "unit-picked", vec![json!(0)]); // to seconds: the same wait
    assert_eq!((r.shown("automatic", "delay-text"), r.plugin.stop_delay_s), (json!("120"), 120));
    r.ui("automatic", "delay-committed", vec![json!("90")]);
    assert_eq!(r.plugin.stop_delay_s, 90);
    r.ui("automatic", "unit-picked", vec![json!(1)]); // 90 s rounds to 2 min
    assert_eq!((r.shown("automatic", "delay-text"), r.plugin.stop_delay_s), (json!("2"), 120));
    r.ui("automatic", "unit-picked", vec![json!(0)]);
    assert_eq!(r.plugin.range(), (10, 3600));
    r.ui("automatic", "delay-committed", vec![json!("45")]);
    assert_eq!(r.plugin.stop_delay_s, 45);
    assert!(r.host.calls.iter().any(|c| c == "schedule_save"));
    r.ui("automatic", "shown", vec![]);
    assert_eq!((r.shown("automatic", "unit-current"), r.shown("automatic", "delay-text")), (json!(0), json!("45")));
    r.ui("automatic", "unit-picked", vec![json!(1)]);
    assert_eq!(r.plugin.range(), (1, 60));
}

#[test]
fn the_wait_box_holds_to_its_range_and_steps_like_a_spin_box() {
    let mut r = Rig::new();
    r.configure(json!({"stop_idle": "any", "stop_delay_s": 30}));
    r.ui("automatic", "shown", vec![]);
    r.ui("automatic", "delay-committed", vec![json!("3")]); // under the shortest wait
    assert_eq!((r.plugin.stop_delay_s, r.shown("automatic", "delay-text")), (10, json!("10")));
    r.ui("automatic", "delay-committed", vec![json!(" 99999 ")]);
    assert_eq!((r.plugin.stop_delay_s, r.shown("automatic", "delay-text")), (3600, json!("3600")));
    r.ui("automatic", "delay-committed", vec![json!("soon")]); // not a number: it goes back to what it was
    assert_eq!((r.plugin.stop_delay_s, r.shown("automatic", "delay-text")), (3600, json!("3600")));
    r.ui("automatic", "delay-step", vec![json!(-1)]);
    assert_eq!(r.plugin.stop_delay_s, 3599);
    r.ui("automatic", "delay-step", vec![json!(1)]);
    r.ui("automatic", "delay-step", vec![json!(1)]);
    assert_eq!(r.plugin.stop_delay_s, 3600);
    // In minutes a step is a minute
    r.ui("automatic", "unit-picked", vec![json!(1)]);
    assert_eq!(r.shown("automatic", "delay-text"), json!("60"));
    r.ui("automatic", "delay-step", vec![json!(-1)]);
    assert_eq!(r.plugin.stop_delay_s, 3540);
}

#[test]
fn in_a_dev_profile_the_sign_in_switch_is_off_with_a_reason() {
    let r = Rig::new();
    let props = r.plugin.sign_in_props();
    let dev = telescope_core::config::dev_profile().is_some();
    assert_eq!(props["open-enabled"], json!(!dev));
    assert_eq!(props["open-tip"] == json!("Not available in a dev profile."), dev);
}

#[test]
fn it_is_a_streaming_plugin_that_can_be_switched_off() {
    let info = StartupPlugin::new(Arc::new(FakeAutostart::default())).info();
    assert_eq!((info.name.as_str(), info.category.as_str(), info.icon.as_str()), ("startup", "streaming", "monitor"));
    assert!(!info.required && !info.optional && !info.device_local);
}

#[test]
fn delay_text_says_whole_minutes_as_minutes() {
    assert_eq!((delay_text(15), delay_text(90), delay_text(120), delay_text(3600)), ("15 s".to_string(), "90 s".to_string(), "2 min".to_string(), "60 min".to_string()));
}

#[test]
fn shutting_down_stops_the_wait() {
    let mut r = Rig::new();
    r.watched(false);
    r.stop_idle("any");
    r.stream_runs();
    assert!(r.counting());
    r.plugin.shutdown(&mut r.host);
    assert!(!r.counting());
}

#[test]
fn stopping_a_stream_it_started_is_the_default_and_a_saved_choice_wins() {
    let r = Rig::new();
    assert_eq!(r.plugin.stop_idle, "own");
    assert_eq!(r.plugin.get_config()["stop_idle"], "own");
    // What a settings file with no choice in it gives, and what one that chose each gives
    for (saved, stop_idle) in [(json!({}), "own"), (json!({"start_on": "ready"}), "own"), (json!({"stop_idle": "off"}), "off"), (json!({"start_on": "ready", "stop_idle": "any"}), "any")] {
        let mut r = Rig::new();
        r.configure(saved.clone());
        assert_eq!(r.plugin.stop_idle, stop_idle, "{saved}");
    }
}
