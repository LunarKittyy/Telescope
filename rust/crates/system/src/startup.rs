//! The Startup plugin (plugins/startup.py): starting and stopping on its own, and opening Telescope at sign-in.
//!
//! The Automatic streaming card in Settings picks when a stream starts by itself (never, as soon as the phone is ready, or when
//! an app opens the camera) and when one stops by itself (never, or once no app has read the camera for a while: only a stream it
//! started, or any stream). Starting when the phone is ready happens once each time the phone becomes ready, and keeps the phone
//! checked with the window hidden (otherwise it's checked only while the window is on screen or an app reads the camera); after
//! a Stop it waits until the phone goes away and comes back, so Stop sticks. Starting for an app happens once while that app
//! reads the camera; after a Stop it waits until the app lets go and something opens the camera again. The idle stop never acts
//! before the camera watch has reported at least once, so a machine where it can't tell whether an app reads the camera never has
//! its streams stopped under it. With the phone mic on, the idle stop turns only the camera off and the mic keeps streaming (a
//! call that switched its camera off still hears you); the camera comes back on when an app opens it again, and Stop ends the
//! stream.

use telescope_core::i18n::Fields;
use serde_json::{json, Value};
use std::sync::Arc;
use telescope_core::actions::round_half_even;
use telescope_core::event::Event;
use telescope_core::host::Host;
use telescope_core::i18n::{n_, tr, tr_ctx};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo};
use telescope_core::system::Autostart;
use telescope_core::ui::{Issue, SettingsSection, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "startup";
const UI: &str = include_str!("../ui/startup.slint");
/// The timer for the wait before an idle stop.
const IDLE_STOP: &str = "idle-stop";
const ISSUE: &str = "startup";

pub const START_OFF: &str = "off";
pub const START_READY: &str = "ready";
pub const START_WATCHED: &str = "watched";
pub const STOP_OFF: &str = "off";
pub const STOP_OWN: &str = "own";
pub const STOP_ANY: &str = "any";
pub const START_CHOICES: [(&str, &str); 3] = [
    (START_OFF, n_("Only when I press Start")),
    (START_READY, n_("When the phone is ready")),
    (START_WATCHED, n_("When an app opens the camera")),
];
pub const STOP_CHOICES: [(&str, &str); 3] = [
    (STOP_OFF, n_("Only when I press Stop")),
    (STOP_OWN, n_("When no app uses the camera, if it started the stream")),
    (STOP_ANY, n_("When no app uses the camera, even if I started it")),
];
/// What the wait is counted in: (text, seconds in one).
const UNITS: [(&str, i64); 2] = [(n_("seconds"), 1), (n_("minutes"), 60)];

/// The shortest wait is longer than the slowest camera watch takes to notice an app (the /proc scan runs every 5 s, and Windows
/// waits 3 s before it counts a reader as gone), so a stream isn't stopped under an app just opening it.
pub const MIN_STOP_DELAY_S: i64 = 10;
pub const MAX_STOP_DELAY_S: i64 = 3600;
pub const DEFAULT_STOP_DELAY_S: i64 = 15;

/// "90 s", "2 min": whole minutes read as minutes.
pub fn delay_text(seconds: i64) -> String {
    if seconds % 60 == 0 { tr("{n} min").field("n", (seconds / 60).to_string()) } else { tr("{n} s").field("n", seconds.to_string()) }
}

/// A saved wait held to its range; anything that isn't a whole number (a boolean included) is the default.
pub fn clamp_delay(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.as_i64().unwrap_or(i64::MAX).clamp(MIN_STOP_DELAY_S, MAX_STOP_DELAY_S),
        _ => DEFAULT_STOP_DELAY_S,
    }
}

fn english(choices: &[(&'static str, &'static str)], key: &str) -> &'static str {
    choices.iter().find(|(k, _)| *k == key).map_or("", |(_, text)| text)
}

pub struct StartupPlugin {
    autostart: Arc<dyn Autostart>,
    start_on: &'static str,
    stop_idle: &'static str,
    stop_delay_s: i64,
    notify_stop: bool,
    /// Already started (or stopped) for this arrival of the phone.
    held: bool,
    /// What phone_wanted last said.
    wanted_sent: Option<(bool, bool)>,
    phone: Option<String>,
    ready: bool,
    watched: bool,
    /// The window is on screen (Event::WindowShown).
    shown: bool,
    /// The camera watch has reported at least once.
    watch_known: bool,
    /// Already started (or stopped) while this app reads the camera.
    watch_held: bool,
    /// Asked the host to start; the next stream_started is ours.
    starting_own: bool,
    /// The running stream is one it started.
    started_own: bool,
    /// Several cameras streamed since the last full stop: the one left may go to an extra camera, which the watch doesn't see, so
    /// nothing stops by itself until everything has.
    several: bool,
    /// The wait before an idle stop is counting, and how long it was set for.
    idle_active: bool,
    idle_ms: u64,
    /// The Wait before stopping box as it reads: the number in the unit picked.
    unit: usize,
    delay_value: i64,
    /// set_config has no host: the tray and the timer hear the settings with the next hook that has one.
    pending: bool,
}

impl StartupPlugin {
    pub fn new(autostart: Arc<dyn Autostart>) -> StartupPlugin {
        StartupPlugin {
            autostart,
            start_on: START_OFF,
            stop_idle: STOP_OFF,
            stop_delay_s: DEFAULT_STOP_DELAY_S,
            notify_stop: true,
            held: false,
            wanted_sent: None,
            phone: None,
            ready: false,
            watched: false,
            shown: true,
            watch_known: false,
            watch_held: false,
            starting_own: false,
            started_own: false,
            several: false,
            idle_active: false,
            idle_ms: 0,
            unit: 0,
            delay_value: DEFAULT_STOP_DELAY_S,
            pending: false,
        }
    }

    pub fn start_on(&self) -> &str {
        self.start_on
    }

    pub fn stop_idle(&self) -> &str {
        self.stop_idle
    }

    // ── Settings ──────────────────────────────────────────────────────

    pub fn set_start_on(&mut self, host: &mut dyn Host, mode: &str) {
        let Some((mode, _)) = START_CHOICES.iter().find(|(m, _)| *m == mode) else { return };
        if *mode == self.start_on {
            return;
        }
        self.start_on = mode;
        self.set_held(host, false);
        self.watch_held = false;
        self.forget_own_stream(host); // a stream it started is the user's now
        self.keep_in_tray(host);
        host.schedule_save();
        self.maybe_start_for_watch(host);
        self.sync_idle_timer(host);
    }

    pub fn set_stop_idle(&mut self, host: &mut dyn Host, mode: &str) {
        let Some((mode, _)) = STOP_CHOICES.iter().find(|(m, _)| *m == mode) else { return };
        if *mode == self.stop_idle {
            return;
        }
        self.stop_idle = mode;
        host.schedule_save();
        self.sync_idle_timer(host);
    }

    pub fn set_stop_delay(&mut self, host: &mut dyn Host, seconds: i64) {
        let seconds = clamp_delay(Some(&json!(seconds)));
        if seconds == self.stop_delay_s {
            return;
        }
        self.stop_delay_s = seconds;
        host.schedule_save();
        if self.idle_active {
            self.start_idle_timer(host); // counts the new wait from now
        }
    }

    pub fn set_notify_stop(&mut self, host: &mut dyn Host, on: bool) {
        if on != self.notify_stop {
            self.notify_stop = on;
            host.schedule_save();
        }
    }

    /// Closing the window mustn't quit what's waiting for the phone or for an app.
    fn keep_in_tray(&mut self, host: &mut dyn Host) {
        host.set_keep_in_tray(self.start_on != START_OFF);
        self.sync_wanted(host);
    }

    fn set_held(&mut self, host: &mut dyn Host, held: bool) {
        self.held = held;
        self.sync_wanted(host);
    }

    /// Waiting to start when ready: the phone is checked from the tray too. Once held, only the phone going away (which lifts the
    /// hold) matters, so a phone left waiting all day is checked slowly.
    fn sync_wanted(&mut self, host: &mut dyn Host) {
        let wanted = (self.start_on == START_READY, self.held);
        if self.wanted_sent != Some(wanted) {
            self.wanted_sent = Some(wanted);
            host.emit(Event::PhoneWanted { wanted: wanted.0, held: wanted.1 });
        }
    }

    pub fn set_open_at_sign_in(&mut self, host: &mut dyn Host, on: bool) {
        match self.autostart.set_enabled(on, true) {
            Ok(()) => host.clear_issue(Some(ISSUE), None),
            Err(detail) => host.show_issue(ISSUE, Issue { title: detail, text: String::new(), actions: Vec::new(), kind: "err".into(), details: String::new(), tell_dismiss: false }),
        }
    }

    // ── Starting when the phone is ready ──────────────────────────────

    fn on_phone_ready(&mut self, host: &mut dyn Host, phone_id: &str, ready: bool) {
        if self.phone.as_deref() != Some(phone_id) {
            if self.phone.is_some() {
                self.set_held(host, false); // another phone: a new arrival (the first report just names it)
            }
            self.phone = Some(phone_id.to_string());
        }
        self.ready = ready;
        if !ready {
            self.set_held(host, false); // gone: the next time it's ready counts as a new arrival
            return;
        }
        if self.start_on == START_READY && !self.held && !host.is_streaming() && !host.is_starting() {
            // Held before starting, so a start that fails isn't retried on every status check
            self.set_held(host, true);
            self.start_own(host, "Starting the stream now the phone is ready");
        }
        self.maybe_start_for_watch(host);
    }

    fn on_device_changed(&mut self, host: &mut dyn Host) {
        self.set_held(host, false);
        self.ready = false; // the new phone's first status check says whether it's ready
    }

    // ── Starting when an app opens the camera ─────────────────────────

    fn on_window_shown(&mut self, shown: bool) {
        self.shown = shown;
        self.forget_ready_if_unchecked();
    }

    /// The phone isn't checked while the window is hidden and no app reads the camera (unless starting when ready, which never
    /// reads `ready`), so what it said last can go stale. Once checks resume, the first one answers with phone_ready and
    /// anything waiting on it starts then.
    fn forget_ready_if_unchecked(&mut self) {
        if !self.shown && !self.watched {
            self.ready = false;
        }
    }

    fn on_camera_watched(&mut self, host: &mut dyn Host, watched: bool) {
        self.watch_known = true;
        self.watched = watched;
        self.forget_ready_if_unchecked();
        log::info!("An app {} reading the camera", if watched { "started" } else { "stopped" });
        if watched {
            if host.is_streaming() && host.is_camera_off_auto() {
                log::info!("An app opened the camera again; turning the phone's camera back on");
                host.set_camera_on(true, true);
            }
            self.maybe_start_for_watch(host);
        } else {
            self.watch_held = false; // the next app to open the camera counts as new
            if self.stop_idle == STOP_OWN && host.is_streaming() && !self.owns_stream() {
                log::info!("The stream wasn't started automatically, so it keeps going");
            }
        }
        self.sync_idle_timer(host);
    }

    fn maybe_start_for_watch(&mut self, host: &mut dyn Host) {
        if !(self.start_on == START_WATCHED && self.watched && self.ready) || self.watch_held {
            return;
        }
        if host.is_streaming() || host.is_starting() {
            return; // someone else's start; it isn't this one's to claim
        }
        self.watch_held = true;
        self.start_own(host, "Starting the stream for the app reading the camera");
    }

    fn start_own(&mut self, host: &mut dyn Host, why: &str) {
        // Marked before asking: a start that fails straight away says so as soon as the host gets to it
        self.starting_own = true;
        log::info!("{why}");
        host.start_stream(false);
    }

    // ── The stream coming and going ───────────────────────────────────

    fn on_stream_started(&mut self, host: &mut dyn Host) {
        self.started_own = self.starting_own;
        self.starting_own = false;
        if self.idle_stop_applies() && !self.watch_known {
            log::info!("Can't tell yet whether an app reads the camera, so the stream won't stop by itself");
        }
        self.sync_idle_timer(host);
    }

    fn on_streams_changed(&mut self, host: &mut dyn Host, count: u32) {
        if count > 1 {
            self.several = true;
        }
        self.sync_idle_timer(host);
    }

    fn on_stream_start_failed(&mut self, host: &mut dyn Host) {
        self.starting_own = false;
        self.sync_idle_timer(host);
    }

    fn on_stream_stopped(&mut self, host: &mut dyn Host) {
        self.set_held(host, true);
        if host.is_restarting() {
            // The same stream back in a moment (a reconnect or camera resize): still one it started
            self.starting_own = self.starting_own || self.started_own;
            self.started_own = false;
            return;
        }
        self.forget_own_stream(host);
        self.several = false;
        if self.watched {
            self.watch_held = true; // stopped while an app reads the camera: wait for it to let go
        }
        self.sync_idle_timer(host);
    }

    // ── Stopping once nothing reads the camera ────────────────────────

    fn owns_stream(&self) -> bool {
        self.started_own || self.starting_own
    }

    fn idle_stop_applies(&self) -> bool {
        self.stop_idle == STOP_ANY || (self.stop_idle == STOP_OWN && self.owns_stream())
    }

    /// Whether the wait before an idle stop should be running right now. Every doubt means no.
    fn counting(&self, host: &dyn Host) -> bool {
        self.idle_stop_applies() && self.watch_known && !self.watched
            && (host.is_streaming() || self.starting_own) && host.is_camera_on()
            && host.stream_count() <= 1 && !self.several // each camera is somebody's
    }

    fn start_idle_timer(&mut self, host: &mut dyn Host) {
        self.idle_ms = self.stop_delay_s as u64 * 1000;
        self.idle_active = true;
        host.schedule(self.idle_ms, IDLE_STOP);
    }

    fn stop_idle_timer(&mut self, host: &mut dyn Host) {
        if std::mem::take(&mut self.idle_active) {
            host.cancel_schedule(IDLE_STOP);
        }
    }

    fn sync_idle_timer(&mut self, host: &mut dyn Host) {
        if !self.counting(host) {
            self.stop_idle_timer(host);
        } else if !self.idle_active {
            self.start_idle_timer(host);
        }
    }

    /// The wait is over.
    fn stop_idle_now(&mut self, host: &mut dyn Host) {
        self.idle_active = false;
        if !self.counting(host) {
            return; // something changed since the wait started
        }
        let running = host.is_streaming() || host.is_starting();
        if host.is_streaming() && host.can_turn_camera_off().0 {
            // The mic is on: a call that switched its camera off still hears it, so only the camera goes
            log::info!("No app has read the camera for {}; turning the phone's camera off", delay_text(self.stop_delay_s));
            host.set_camera_on(false, true);
            return;
        }
        self.forget_own_stream(host);
        if !running {
            return; // its start already failed; nothing to stop
        }
        log::info!("No app has read the camera for {}; stopping the stream", delay_text(self.stop_delay_s));
        host.stop_stream(None); // also stops a start still waking the phone
        if self.notify_stop {
            host.send_notification(
                &tr("Telescope stopped streaming"),
                &tr("No app used the camera for {time}.").field("time", delay_text(self.stop_delay_s)),
                false,
            );
        }
    }

    fn forget_own_stream(&mut self, host: &mut dyn Host) {
        self.starting_own = false;
        self.started_own = false;
        self.stop_idle_timer(host);
    }

    // ── The settings window ───────────────────────────────────────────

    fn scale(&self) -> i64 {
        UNITS[self.unit].1
    }

    /// The Wait before stopping box as the wait reads: whole minutes in minutes.
    fn show_wait(&mut self) {
        self.unit = if self.stop_delay_s % 60 == 0 { 1 } else { 0 };
        self.delay_value = self.stop_delay_s / self.scale();
    }

    /// The box holds at least the shortest wait and at most the longest, in its unit (a QSpinBox's range).
    fn range(&self) -> (i64, i64) {
        let scale = self.scale();
        (-(-MIN_STOP_DELAY_S).div_euclid(scale), MAX_STOP_DELAY_S / scale)
    }

    /// The box's value as seconds.
    fn seconds(&self) -> i64 {
        clamp_delay(Some(&json!(self.delay_value * self.scale())))
    }

    fn sign_in_props(&self) -> Value {
        let dev = telescope_core::config::dev_profile().is_some();
        json!({
            "open-checked": self.autostart.enabled(),
            "open-enabled": !dev,
            "open-tip": if dev { tr("Not in a dev profile") } else { String::new() },
        })
    }

    fn automatic_props(&self) -> Value {
        let position = |choices: &[(&str, &str)], key: &str| choices.iter().position(|(k, _)| *k == key).unwrap_or(0);
        let stops = self.stop_idle != STOP_OFF; // the wait and the notice do nothing without an idle stop
        json!({
            "start-title": tr_ctx("setting", "Start"), "stop-title": tr_ctx("setting", "Stop"),
            "start-items": START_CHOICES.iter().map(|(_, t)| tr(t)).collect::<Vec<_>>(),
            "start-current": position(&START_CHOICES, self.start_on),
            "stop-items": STOP_CHOICES.iter().map(|(_, t)| tr(t)).collect::<Vec<_>>(),
            "stop-current": position(&STOP_CHOICES, self.stop_idle),
            "stops": stops,
            "delay-title": tr("Wait before stopping"),
            "delay-sub": "",
            "delay-text": self.delay_value.to_string(),
            "unit-items": UNITS.iter().map(|(t, _)| tr(t)).collect::<Vec<_>>(),
            "unit-current": self.unit,
            "notify-checked": self.notify_stop,
        })
    }

    /// Both sections as the settings say.
    fn refresh(&mut self, host: &mut dyn Host) {
        self.show_wait();
        host.set_ui_props("sign-in", &self.sign_in_props());
        host.set_ui_props("automatic", &self.automatic_props());
    }

    fn show_automatic(&self, host: &mut dyn Host) {
        host.set_ui_props("automatic", &self.automatic_props());
    }

    fn set_delay_from_box(&mut self, host: &mut dyn Host, value: i64) {
        let (lo, hi) = self.range();
        self.delay_value = value.clamp(lo, hi);
        let seconds = self.seconds();
        self.set_stop_delay(host, seconds);
        self.show_automatic(host);
    }

    fn pick_unit(&mut self, host: &mut dyn Host, unit: usize) {
        if unit >= UNITS.len() || unit == self.unit {
            return;
        }
        // Keep roughly the same wait: 90 s becomes 2 min, 2 min becomes 120 s
        let seconds = self.delay_value * self.scale();
        self.unit = unit;
        let (lo, hi) = self.range();
        self.delay_value = (round_half_even(seconds as f64 / self.scale() as f64) as i64).clamp(lo, hi);
        let seconds = self.seconds();
        self.set_stop_delay(host, seconds);
        self.show_automatic(host);
    }

    /// Catches up with what set_config loaded, once there's a host to tell.
    fn catch_up(&mut self, host: &mut dyn Host) {
        if std::mem::take(&mut self.pending) {
            self.keep_in_tray(host);
            self.sync_idle_timer(host);
        }
    }
}

impl Plugin for StartupPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Start and stop by itself").into(),
            about: n_("Opening Telescope when you sign in, and Automatic streaming.").into(),
            when_off: n_("Without it Automatic streaming stops and both it and the sign-in option leave Settings. Telescope still opens at sign-in if it did before.").into(),
            category: "streaming".into(),
            icon: "monitor".into(),
            ..Default::default()
        }
    }

    fn on_plugins_ready(&mut self, host: &mut dyn Host) {
        self.catch_up(host);
    }

    fn shutdown(&mut self, host: &mut dyn Host) {
        self.stop_idle_timer(host);
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        self.catch_up(host);
        match event {
            Event::PhoneReady { phone_id, ready } => self.on_phone_ready(host, phone_id, *ready),
            Event::StreamStarted { .. } => self.on_stream_started(host),
            Event::StreamStopped => self.on_stream_stopped(host),
            Event::StreamStartFailed => self.on_stream_start_failed(host),
            Event::DeviceChanged { .. } => self.on_device_changed(host),
            Event::StreamsChanged { count } => self.on_streams_changed(host, *count),
            Event::CameraWatched { watched } => self.on_camera_watched(host, *watched),
            Event::WindowShown { shown } => self.on_window_shown(*shown),
            Event::TimerFired { owner, token } if owner == NAME && token == IDLE_STOP => self.stop_idle_now(host),
            _ => {}
        }
    }

    fn settings_sections(&mut self) -> Vec<SettingsSection> {
        self.show_wait();
        let section = |id: &str, tab: &str, component: &str, props: Value| SettingsSection {
            id: id.into(),
            tab: tab.into(),
            ui: UiSpec { source: SlintSource::Text(UI.into()), component: component.into(), props },
            order: 0,
            tab_title: String::new(),
            keywords: String::new(),
            footer: None,
        };
        vec![
            section("sign-in", "general", "StartupSettings", self.sign_in_props()),
            section("automatic", "streaming", "AutomaticSettings", self.automatic_props()),
        ]
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        self.catch_up(host);
        let index = event.args.first().and_then(Value::as_u64).map(|i| i as usize);
        let on = event.args.first().and_then(Value::as_bool).unwrap_or(false);
        match (event.ui.as_str(), event.callback.as_str()) {
            ("sign-in", "open-toggled") => self.set_open_at_sign_in(host, on),
            ("sign-in", "shown") | ("automatic", "shown") => self.refresh(host),
            ("automatic", "start-picked") => {
                if let Some((mode, _)) = index.and_then(|i| START_CHOICES.get(i)) {
                    self.set_start_on(host, mode);
                }
            }
            ("automatic", "stop-picked") => {
                if let Some((mode, _)) = index.and_then(|i| STOP_CHOICES.get(i)) {
                    self.set_stop_idle(host, mode);
                    self.show_automatic(host); // the wait and the notice come on with an idle stop
                }
            }
            ("automatic", "unit-picked") => {
                if let Some(i) = index {
                    self.pick_unit(host, i);
                }
            }
            ("automatic", "delay-committed") => {
                let typed = event.args.first().and_then(Value::as_str).unwrap_or("").trim();
                match telescope_core::names::whole_number(typed) {
                    Some(value) => self.set_delay_from_box(host, value),
                    None => self.show_automatic(host), // not a number: the box says what it was
                }
            }
            ("automatic", "delay-step") => {
                let by = event.args.first().and_then(Value::as_i64).unwrap_or(0);
                let value = self.delay_value + by;
                self.set_delay_from_box(host, value);
            }
            ("automatic", "notify-toggled") => self.set_notify_stop(host, on),
            _ => {}
        }
    }

    fn diagnostics(&self) -> Vec<(String, String)> {
        vec![
            ("Start streaming".into(), english(&START_CHOICES, self.start_on).into()),
            ("Stop streaming".into(), english(&STOP_CHOICES, self.stop_idle).into()),
            ("Wait before stopping".into(), delay_text(self.stop_delay_s)),
            ("Tell me when it stops a stream".into(), if self.notify_stop { "on" } else { "off" }.into()),
            ("Stream started automatically".into(), if self.owns_stream() { "yes" } else { "no" }.into()),
        ]
    }

    fn get_config(&self) -> Value {
        json!({"start_on": self.start_on, "stop_idle": self.stop_idle, "stop_delay_s": self.stop_delay_s, "notify_stop": self.notify_stop})
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        let choice = |choices: &[(&'static str, &'static str)], key: &str| {
            cfg.get(key).and_then(Value::as_str).and_then(|v| choices.iter().find(|(k, _)| *k == v).map(|(k, _)| *k))
        };
        if cfg.get("start_on").is_some() || cfg.get("stop_idle").is_some() {
            // A list or a table can't be looked up among the choices: Python's set_config fails on it, and the host then keeps
            // the settings that load on their own
            for key in ["start_on", "stop_idle"] {
                if matches!(cfg.get(key), Some(Value::Array(_) | Value::Object(_))) {
                    return Err(ConfigRejected(format!("{key} isn't a choice")));
                }
            }
            self.start_on = choice(&START_CHOICES, "start_on").unwrap_or(START_OFF);
            self.stop_idle = choice(&STOP_CHOICES, "stop_idle").unwrap_or(STOP_OFF);
        } else if cfg.get("watch_stream") == Some(&Value::Bool(true)) {
            // The two checkboxes from before; watching won when both were on
            (self.start_on, self.stop_idle) = (START_WATCHED, STOP_OWN);
        } else if cfg.get("auto_stream") == Some(&Value::Bool(true)) {
            (self.start_on, self.stop_idle) = (START_READY, STOP_OFF);
        } else {
            (self.start_on, self.stop_idle) = (START_OFF, STOP_OFF);
        }
        self.stop_delay_s = clamp_delay(cfg.get("stop_delay_s"));
        self.notify_stop = cfg.get("notify_stop") != Some(&Value::Bool(false));
        self.pending = true;
        Ok(())
    }
}

#[cfg(test)]
#[path = "startup_tests.rs"]
mod tests;
