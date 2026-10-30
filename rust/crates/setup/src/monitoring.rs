//! Monitoring (plugins/monitoring.py): the Phone card with the battery and temperature of the stream the panels show, and
//! warnings when either crosses its threshold. Every 15 s it asks each phone stream for its state, the ones behind the panels
//! too, and judges those by the thresholds saved for that phone. A warning notifies once per crossing (it re-arms 5 below
//! or above the line); Stop the stream stops on every reading past the line, until Start anyway lets it carry on.

use crate::py;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use telescope_core::event::Event;
use telescope_core::host::Host;
use telescope_core::i18n::{n_, number, percent, tr, tr_ctx};
use telescope_core::plugin::{ConfigRejected, Plugin, PluginInfo, StreamInfo};
use telescope_core::ui::{CardSpec, Issue, IssueAction, IssueDoes, SlintSource, UiEvent, UiSpec};

pub const NAME: &str = "monitoring";
const UI: &str = concat!(include_str!("../ui/parts.slint"), "\n", include_str!("../ui/monitoring.slint"));
const POLL: &str = "poll";
pub const POLL_MS: u64 = 15_000;
const START_ANYWAY: &str = "start-anyway";

pub const BATTERY_RANGE: (i64, i64) = (5, 95);
pub const TEMP_RANGE: (i64, i64) = (35, 65);

/// A stream's alert settings (_DEFAULTS and _thresholds).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thresholds {
    pub battery_alert: i64,
    pub temp_alert: i64,
    pub battery_notify: bool,
    pub battery_stop: bool,
    pub temp_notify: bool,
    pub temp_stop: bool,
}

impl Default for Thresholds {
    fn default() -> Self {
        Thresholds { battery_alert: 20, temp_alert: 45, battery_notify: true, battery_stop: false, temp_notify: true, temp_stop: false }
    }
}

impl Thresholds {
    /// A stream's saved alert settings, the defaults for what's missing or doesn't make sense.
    pub fn from_saved(saved: &Value) -> Thresholds {
        let mut t = Thresholds::default();
        let flag = |key: &str, default: bool| saved.get(key).and_then(Value::as_bool).unwrap_or(default);
        let whole = |key: &str, default: i64| saved.get(key).and_then(py::whole).unwrap_or(default);
        t.battery_alert = whole("battery_alert", t.battery_alert);
        t.temp_alert = whole("temp_alert", t.temp_alert);
        t.battery_notify = flag("battery_notify", t.battery_notify);
        t.battery_stop = flag("battery_stop", t.battery_stop);
        t.temp_notify = flag("temp_notify", t.temp_notify);
        t.temp_stop = flag("temp_stop", t.temp_stop);
        t
    }
}

/// (battery level, charging, temperature) from a phone state, or None without one that makes sense.
pub fn reading(state: &Value) -> Option<(i64, bool, f64)> {
    let level = py::int(state.get("battery")?)?;
    let charging = state.get("charging").is_none_or(py::truthy);
    let temp = match state.get("battery_temp_c") {
        None => 0.0,
        Some(v) => py::float(v)?,
    };
    Some((level, charging, temp))
}

/// What was already warned about for one stream.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Warned {
    pub batt: bool,
    pub temp: bool,
    pub last: Option<i64>,
}

/// One reading on the card: value, note, status kind and icon name.
#[derive(Clone, Debug, PartialEq)]
pub struct Pill {
    pub value: String,
    pub note: String,
    pub kind: String,
    pub icon: String,
}

impl Pill {
    fn empty(icon: &str) -> Pill {
        Pill { value: "—".into(), note: String::new(), kind: "status_dim".into(), icon: icon.into() }
    }
}

pub struct MonitoringPlugin {
    pub battery_alert: i64,
    pub temp_alert: i64,
    pub battery_notify: bool,
    pub battery_stop: bool,
    pub temp_notify: bool,
    pub temp_stop: bool,
    /// The shown stream's url while there is one.
    pub shown: Option<String>,
    /// Its StreamInfo::session (Python's ctrl), which tells streams apart: the url moves with a hand-over.
    pub shown_session: Option<u64>,
    /// The shown stream's source.
    pub sid: Option<String>,
    pub warned: Warned,
    /// What's been warned about for streams the panels don't show, with the session of the one left behind (None when it
    /// was first read from behind).
    pub others: HashMap<String, (Option<u64>, Warned)>,
    /// Source id (None: a source without one): what was started anyway, not stopped again until that's fine.
    pub allowed: HashMap<Option<String>, HashSet<&'static str>>,
    /// Banner key: (source, low, hot) of a stop that Start anyway would undo.
    stops: HashMap<String, (Option<String>, bool, bool)>,
    pub polling: bool,
    pub battery: Pill,
    pub temp: Pill,
    phones: u32,
    source: String,
}

impl Default for MonitoringPlugin {
    fn default() -> Self {
        MonitoringPlugin::new()
    }
}

impl MonitoringPlugin {
    pub fn new() -> MonitoringPlugin {
        let t = Thresholds::default();
        MonitoringPlugin {
            battery_alert: t.battery_alert,
            temp_alert: t.temp_alert,
            battery_notify: t.battery_notify,
            battery_stop: t.battery_stop,
            temp_notify: t.temp_notify,
            temp_stop: t.temp_stop,
            shown: None,
            sid: None,
            warned: Warned::default(),
            shown_session: None,
            others: HashMap::new(),
            allowed: HashMap::new(),
            stops: HashMap::new(),
            polling: false,
            battery: Pill::empty("battery"),
            temp: Pill::empty("thermometer"),
            phones: 0,
            source: String::new(),
        }
    }

    fn thresholds(&self) -> Thresholds {
        Thresholds {
            battery_alert: self.battery_alert,
            temp_alert: self.temp_alert,
            battery_notify: self.battery_notify,
            battery_stop: self.battery_stop,
            temp_notify: self.temp_notify,
            temp_stop: self.temp_stop,
        }
    }

    fn spin_text(value: i64, suffix_ctx: &str) -> String {
        format!("{value}{}", tr_ctx("after a number", suffix_ctx))
    }

    fn props(&self) -> Value {
        json!({
            "active": self.phones > 0,
            "phone-only-hidden": !self.source.is_empty(),
            "battery-value": self.battery.value, "battery-note": self.battery.note, "battery-kind": self.battery.kind,
            "battery-icon": self.battery.icon,
            "temp-value": self.temp.value, "temp-note": self.temp.note, "temp-kind": self.temp.kind,
            "batt-text": Self::spin_text(self.battery_alert, "%"),
            "temp-text": Self::spin_text(self.temp_alert, " °C"),
            "batt-notify": self.battery_notify, "batt-stop": self.battery_stop,
            "temp-notify": self.temp_notify, "temp-stop": self.temp_stop,
        })
    }

    fn show(&self, host: &mut dyn Host) {
        host.set_ui_props("card", &self.props());
    }

    fn start_polling(&mut self, host: &mut dyn Host) {
        if !self.polling {
            self.polling = true;
            host.schedule(POLL_MS, POLL);
        }
    }

    fn stop_polling(&mut self, host: &mut dyn Host) {
        if self.polling {
            self.polling = false;
            host.cancel_schedule(POLL);
        }
    }

    /// The panels show this stream now; kept is what was already warned about for it, behind another one.
    fn show_stream(&mut self, host: &mut dyn Host, stream: &StreamInfo, kept: Option<Warned>) {
        self.shown = Some(stream.url.clone());
        self.shown_session = Some(stream.session);
        self.sid = host.focused_source_id();
        if let Some(sid) = &self.sid {
            self.others.remove(sid);
        }
        self.clear_readings(host);
        self.warned = kept.unwrap_or_default();
    }

    fn forget_shown(&mut self, host: &mut dyn Host) {
        self.shown = None;
        self.clear_readings(host);
    }

    fn clear_readings(&mut self, host: &mut dyn Host) {
        self.battery = Pill::empty("battery");
        self.temp = Pill::empty("thermometer");
        self.show(host);
    }

    /// Each phone stream is asked for its state; the shown one's comes back as phone_state_updated, the others' as
    /// stream_state.
    pub fn poll(&mut self, host: &mut dyn Host) {
        if self.shown.is_some() {
            host.request_phone_state(None);
        }
        let behind = host.background_streams();
        self.others.retain(|sid, _| behind.iter().any(|b| &b.source_id == sid));
        for stream in behind {
            host.request_phone_state(Some(&stream.source_id));
        }
    }

    /// The shown stream's state.
    pub fn on_state(&mut self, host: &mut dyn Host, state: &Value) {
        let Some((level, charging, temp)) = reading(state) else { return }; // the next poll tries again
        self.update_display(host, level, charging, temp);
        self.check_alerts(host, level, charging, temp);
    }

    /// A stream the panels don't show: its own alerts, by the thresholds saved for it.
    pub fn on_other_state(&mut self, host: &mut dyn Host, sid: &str, state: &Value) {
        let Some(name) = host.background_streams().into_iter().find(|b| b.source_id == sid).map(|b| b.name) else {
            return; // stopped, or the panels show it now
        };
        let Some((level, charging, temp)) = reading(state) else { return };
        let (session, mut warned) = self.others.remove(sid).unwrap_or_default();
        let cfg = Thresholds::from_saved(&host.device_config(Some(sid), NAME));
        self.alert(host, &mut warned, level, charging, temp, &cfg, Some(sid), &name);
        self.others.insert(sid.to_string(), (session, warned));
    }

    pub fn update_display(&mut self, host: &mut dyn Host, level: i64, charging: bool, temp_c: f64) {
        let batt_kind = if !charging && level <= self.battery_alert {
            "status_err"
        } else if !charging && level <= self.battery_alert + 10 {
            "status_warn"
        } else {
            "status_ok"
        };
        self.battery = Pill {
            value: percent(level as f64, 0, false),
            note: if charging { tr("Charging") } else { String::new() },
            kind: batt_kind.into(),
            icon: if charging { "battery-charging" } else { "battery" }.into(),
        };
        let temp_kind = if temp_c >= self.temp_alert as f64 {
            "status_err"
        } else if temp_c >= (self.temp_alert - 5) as f64 {
            "status_warn"
        } else {
            "status_ok"
        };
        let feel = match temp_kind { "status_ok" => tr("Cool"), "status_warn" => tr("Warm"), _ => tr("Hot") };
        self.temp = Pill { value: tr("{temp} °C").replace("{temp}", &number(temp_c, 1, false)), note: feel, kind: temp_kind.into(),
                           icon: "thermometer".into() };
        self.show(host);
    }

    /// The shown stream's alerts, by the thresholds on the card.
    pub fn check_alerts(&mut self, host: &mut dyn Host, level: i64, charging: bool, temp_c: f64) {
        let mut warned = self.warned.clone();
        let cfg = self.thresholds();
        self.alert(host, &mut warned, level, charging, temp_c, &cfg, None, "");
        self.warned = warned;
    }

    /// Notify once per crossing of a threshold; stop on every reading past one whose Stop the stream is on. sid and name: a
    /// stream the panels don't show (None: the one they do).
    #[allow(clippy::too_many_arguments)]
    fn alert(&mut self, host: &mut dyn Host, entry: &mut Warned, level: i64, charging: bool, temp_c: f64, cfg: &Thresholds,
             sid: Option<&str>, name: &str) {
        let (batt_thresh, temp_thresh) = (cfg.battery_alert, cfg.temp_alert);
        // A wonky charger may not keep up
        let falling = !charging || entry.last.is_some_and(|last| level < last);
        entry.last = Some(level);
        let low = falling && level <= batt_thresh;
        let hot = temp_c >= temp_thresh as f64;
        let streaming = host.is_streaming();
        let key: Option<String> = sid.map(String::from).or_else(|| self.sid.clone());
        let allowed = self.allowed.entry(key.clone()).or_default();
        if level > batt_thresh + 5 {
            allowed.remove("batt"); // fine again: the next time it runs low stops it as usual
        }
        if temp_c < (temp_thresh - 5) as f64 {
            allowed.remove("temp");
        }
        let stop_low = low && streaming && cfg.battery_stop && !allowed.contains("batt");
        let stop_hot = hot && streaming && cfg.temp_stop && !allowed.contains("temp");
        let phone = if name.is_empty() { tr("Phone") } else { name.to_string() };

        let mut notes = Vec::new();
        if low && (stop_low || !entry.batt) {
            entry.batt = true;
            if cfg.battery_notify {
                let text = if charging {
                    tr("{phone} battery is at {level}% and still dropping despite being plugged in - the charger may not be keeping up.")
                } else {
                    tr("{phone} battery is at {level}%.")
                }.replace("{phone}", &phone).replace("{level}", &level.to_string());
                let text = if stop_low { format!("{text} {}", tr("Stopped streaming.")) } else { text };
                notes.push((tr("Telescope - Low Battery"), text));
            }
        } else if level > batt_thresh + 5 {
            entry.batt = false;
        }

        if hot && (stop_hot || !entry.temp) {
            entry.temp = true;
            if cfg.temp_notify {
                let temp = number(temp_c, 1, false);
                let text = if name.is_empty() {
                    tr("Temperature is {temp} °C.").replace("{temp}", &temp)
                } else {
                    tr("{phone}: temperature is {temp} °C.").replace("{phone}", name).replace("{temp}", &temp)
                };
                let then = if stop_hot { tr("Stopped streaming to let it cool down.") } else { tr("Consider stopping charging or closing other apps.") };
                notes.push((tr("Telescope - Phone Running Hot"), format!("{text} {then}")));
            }
        } else if temp_c < (temp_thresh - 5) as f64 {
            entry.temp = false;
        }

        for (title, text) in notes {
            host.send_notification(&title, &text, false);
        }
        if stop_low || stop_hot {
            log::info!("Stopping the stream: phone {}", if stop_low { "battery low" } else { "too hot" });
            host.stop_stream(sid); // for the shown one it runs on_stream_stop, which clears its state here
            let (title, text) = if stop_low {
                let title = if name.is_empty() {
                    tr("The phone's battery is at {level}%").replace("{level}", &level.to_string())
                } else {
                    tr("{phone}'s battery is at {level}%").replace("{phone}", name).replace("{level}", &level.to_string())
                };
                (title, tr("Streaming stopped, as Monitoring stops it at {level}% or lower.").replace("{level}", &batt_thresh.to_string()))
            } else {
                let temp = number(temp_c, 1, false);
                let title = if name.is_empty() {
                    tr("The phone is at {temp} °C").replace("{temp}", &temp)
                } else {
                    tr("{phone} is at {temp} °C").replace("{phone}", name).replace("{temp}", &temp)
                };
                (title, tr("Streaming stopped to let it cool down, as Monitoring stops it at {temp} °C.").replace("{temp}", &temp_thresh.to_string()))
            };
            let issue_key = format!("stopped:{}", key.as_deref().unwrap_or("None"));
            self.stops.insert(issue_key.clone(), (key.clone(), stop_low, stop_hot));
            host.show_issue(&issue_key, Issue {
                title,
                text,
                actions: vec![IssueAction {
                    label: tr("Start anyway"),
                    does: IssueDoes::StartAgain { source_id: key, before: Some(START_ANYWAY.into()) },
                }],
                kind: "warn".into(),
                details: String::new(),
                tell_dismiss: false,
            });
        }
    }

    /// Start anyway: don't stop it for the same reason until the battery or temperature is fine again (the banner starts it).
    fn start_anyway(&mut self, issue_key: &str) {
        let Some((sid, low, hot)) = self.stops.remove(issue_key) else { return };
        let allowed = self.allowed.entry(sid).or_default();
        if low {
            allowed.insert("batt");
        }
        if hot {
            allowed.insert("temp");
        }
    }

    fn set_battery_alert(&mut self, host: &mut dyn Host, value: i64) {
        let value = value.clamp(BATTERY_RANGE.0, BATTERY_RANGE.1);
        if value != self.battery_alert {
            self.battery_alert = value;
            host.schedule_save();
        }
        self.show(host);
    }

    fn set_temp_alert(&mut self, host: &mut dyn Host, value: i64) {
        let value = value.clamp(TEMP_RANGE.0, TEMP_RANGE.1);
        if value != self.temp_alert {
            self.temp_alert = value;
            host.schedule_save();
        }
        self.show(host);
    }

    /// The setting a switch on the card names, by its id.
    pub fn set_switch(&mut self, host: &mut dyn Host, id: &str, on: bool) {
        let field = match id {
            "batt-notify" => &mut self.battery_notify,
            "batt-stop" => &mut self.battery_stop,
            "temp-notify" => &mut self.temp_notify,
            "temp-stop" => &mut self.temp_stop,
            _ => return,
        };
        if *field != on {
            *field = on;
            host.schedule_save();
        }
        self.show(host);
    }

    /// What a spin box typed reads as: its digits, with or without the suffix.
    fn typed(text: &str) -> Option<i64> {
        let digits: String = text.trim().chars().take_while(|c| c.is_ascii_digit()).collect();
        digits.parse().ok()
    }
}

impl Plugin for MonitoringPlugin {
    fn info(&self) -> PluginInfo {
        PluginInfo {
            name: NAME.into(),
            title: n_("Monitoring").into(),
            about: n_("The phone's battery and temperature, with warnings when they get low or high.").into(),
            when_off: n_("Without it there's no Phone card and nothing warns you about a low battery or a hot phone.").into(),
            category: "streaming".into(),
            icon: "phone".into(),
            device_local: true,
            ..Default::default()
        }
    }

    fn card(&mut self) -> Option<CardSpec> {
        // The phone's own health, beside its camera
        Some(CardSpec { ui: UiSpec { source: SlintSource::Text(UI.into()), component: "MonitoringCard".into(), props: self.props() },
                        region: "right".into() })
    }

    fn on_event(&mut self, host: &mut dyn Host, event: &Event) {
        match event {
            Event::PhoneStateUpdated { state } => self.on_state(host, state),
            Event::StreamState { source_id, state } => self.on_other_state(host, source_id, state),
            Event::TimerFired { token, .. } if token == POLL => {
                self.polling = false;
                if host.is_streaming() {
                    self.poll(host);
                    self.start_polling(host);
                }
            }
            Event::PhonesChanged { count } => {
                self.phones = *count;
                self.show(host);
            }
            Event::SourceSelected { source_id } => {
                self.source = source_id.clone();
                self.show(host);
            }
            _ => {}
        }
    }

    fn on_stream_start(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        self.start_polling(host);
        if self.shown.is_some() && self.sid == host.focused_source_id() {
            self.shown = Some(stream.url.clone());
            self.shown_session = Some(stream.session);
            return; // a reconnect of the same stream: keep the reading shown and don't warn about it again
        }
        self.show_stream(host, stream, None);
    }

    fn on_focus_gained(&mut self, host: &mut dyn Host, stream: &StreamInfo) {
        self.start_polling(host);
        let kept = host.focused_source_id().and_then(|sid| self.others.remove(&sid));
        let kept = kept.filter(|(session, _)| *session == Some(stream.session)).map(|(_, w)| w);
        self.show_stream(host, stream, kept);
    }

    fn on_stream_stop(&mut self, host: &mut dyn Host) {
        if let Some(sid) = &self.sid {
            self.others.remove(sid);
        }
        if !host.is_streaming() {
            self.stop_polling(host);
            self.others.clear();
        }
        self.forget_shown(host);
    }

    fn on_focus_lost(&mut self, host: &mut dyn Host) {
        // The panels moved on: it's watched from behind
        if let (Some(_), Some(sid)) = (&self.shown, self.sid.clone()) {
            self.others.insert(sid, (self.shown_session, self.warned.clone()));
        }
        self.forget_shown(host);
    }

    fn on_ui(&mut self, host: &mut dyn Host, event: &UiEvent) {
        if event.ui != "card" {
            return;
        }
        let text = event.args.first().and_then(Value::as_str).unwrap_or("");
        let by = event.args.first().and_then(Value::as_i64).unwrap_or(0);
        match event.callback.as_str() {
            "batt-committed" => match Self::typed(text) {
                Some(v) => self.set_battery_alert(host, v),
                None => self.show(host),
            },
            "temp-committed" => match Self::typed(text) {
                Some(v) => self.set_temp_alert(host, v),
                None => self.show(host),
            },
            "batt-step" => {
                let v = self.battery_alert + by;
                self.set_battery_alert(host, v);
            }
            "temp-step" => {
                let v = self.temp_alert + by;
                self.set_temp_alert(host, v);
            }
            "alert-toggled" => {
                let on = event.args.get(1).and_then(Value::as_bool).unwrap_or(false);
                self.set_switch(host, text, on);
            }
            _ => {}
        }
    }

    fn on_issue_action(&mut self, _host: &mut dyn Host, key: &str, id: &str) {
        if id == START_ANYWAY {
            self.start_anyway(key);
        }
    }

    fn get_config(&self) -> Value {
        json!({
            "battery_alert": self.battery_alert,
            "temp_alert": self.temp_alert,
            "battery_notify": self.battery_notify,
            "battery_stop": self.battery_stop,
            "temp_notify": self.temp_notify,
            "temp_stop": self.temp_stop,
        })
    }

    fn set_config(&mut self, cfg: &Value) -> Result<(), ConfigRejected> {
        // Like the spin boxes' setValue(int(v)): only a truthy value counts, int() of it has to work, and it's held to the range
        let spin = |key: &str| -> Result<Option<i64>, ConfigRejected> {
            match cfg.get(key) {
                Some(v) if py::truthy(v) => py::int(v).map(Some).ok_or_else(|| ConfigRejected(format!("{key} isn't a number"))),
                _ => Ok(None),
            }
        };
        let (battery, temp) = (spin("battery_alert")?, spin("temp_alert")?);
        if let Some(v) = battery {
            self.battery_alert = v.clamp(BATTERY_RANGE.0, BATTERY_RANGE.1);
        }
        if let Some(v) = temp {
            self.temp_alert = v.clamp(TEMP_RANGE.0, TEMP_RANGE.1);
        }
        // Notifying is on unless switched off; stopping is off unless switched on
        self.battery_notify = cfg.get("battery_notify") != Some(&Value::Bool(false));
        self.temp_notify = cfg.get("temp_notify") != Some(&Value::Bool(false));
        self.battery_stop = cfg.get("battery_stop") == Some(&Value::Bool(true));
        self.temp_stop = cfg.get("temp_stop") == Some(&Value::Bool(true));
        Ok(())
    }
}

#[cfg(test)]
#[path = "monitoring_tests.rs"]
mod tests;
