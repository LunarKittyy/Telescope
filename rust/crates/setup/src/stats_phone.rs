//! The phone's side of the Stats card: what its /v1/state says about its Wi-Fi, heat and encoder. The Wi-Fi band and signal, the battery
//! temperature and the camera's rate are always in the state; the encoder's times, the send queue and Android's thermal status come in
//! `stats`, which a phone only fills while this card asks for it (the `stats` control). An older phone has no `stats` and shows the rest.

use crate::py;
use crate::stats::Row;
use serde_json::Value;
use telescope_core::i18n::{number, tr, Fields};

/// Seconds the phone's reading keeps being asked for while the card is up (its own lease is longer, so one missed ask costs nothing).
pub const ASK_MS: u64 = 3_000;

/// The ids the phone's lines go by in the bottom bar.
pub(crate) const PIN_DELAY: &str = "stats.phone_delay";
pub(crate) const PIN_WIFI: &str = "stats.phone_wifi";
pub(crate) const PIN_TEMPERATURE: &str = "stats.phone_temperature";
pub(crate) const PIN_THROTTLING: &str = "stats.phone_throttling";
pub(crate) const PIN_CAMERA_FPS: &str = "stats.phone_camera_fps";
pub(crate) const PIN_ENCODER_FPS: &str = "stats.phone_encoder_fps";
pub(crate) const PIN_ENCODE: &str = "stats.phone_encode";
pub(crate) const PIN_QUEUE: &str = "stats.phone_queue";

/// Android's thermal status (PowerManager.THERMAL_STATUS_*) at which the phone lowers the frame rate by itself.
const THROTTLING_FROM: i64 = 3;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PhoneReading {
    /// The phone takes the `stats` control (an older one doesn't, and the desktop never sends it).
    pub takes_stats: bool,
    pub wifi_band: Option<String>,
    pub wifi_rssi: Option<i64>,
    pub wifi_mbps: Option<i64>,
    pub battery_temp_c: Option<f64>,
    /// The frame rate asked of the camera, what it makes, and what the encoder puts out.
    pub asked_fps: Option<f64>,
    pub camera_fps: Option<f64>,
    pub encoder_fps: Option<f64>,
    /// Rates the phone capped itself at, for a slow link and for its heat.
    pub dynamic_fps: Option<i64>,
    pub heat_fps: Option<i64>,
    pub encode_ms: Option<f64>,
    pub encode_max_ms: Option<f64>,
    pub queue_ms: Option<f64>,
    pub thermal: Option<i64>,
    pub thermal_headroom: Option<f64>,
}

impl PhoneReading {
    /// From a phone's /v1/state; None for one that says nothing a row could show.
    pub fn from_state(state: &Value) -> Option<PhoneReading> {
        let number_of = |v: Option<&Value>| v.filter(|v| !v.is_boolean()).and_then(py::float).filter(|n| n.is_finite());
        let whole = |v: Option<&Value>| number_of(v).map(|n| n as i64);
        let positive = |n: Option<f64>| n.filter(|n| *n > 0.0);
        let stats = state.get("stats").filter(|s| s.is_object());
        let in_stats = |key: &str| stats.and_then(|s| s.get(key));
        let reading = PhoneReading {
            takes_stats: state.get("takes_stats").and_then(Value::as_bool).unwrap_or(false),
            wifi_band: state.get("wifi_band").and_then(Value::as_str).filter(|b| !b.is_empty()).map(String::from),
            wifi_rssi: whole(state.get("wifi_rssi")),
            wifi_mbps: whole(state.get("wifi_mbps")).filter(|n| *n > 0),
            battery_temp_c: number_of(state.get("battery_temp_c")),
            asked_fps: positive(number_of(state.get("phone_fps"))),
            camera_fps: positive(number_of(state.get("camera_fps"))),
            encoder_fps: positive(number_of(in_stats("encoder_fps"))),
            dynamic_fps: whole(state.get("dynamic_fps")).filter(|n| *n > 0),
            heat_fps: whole(state.get("heat_fps")).filter(|n| *n > 0),
            encode_ms: number_of(in_stats("encode_ms")).filter(|n| *n >= 0.0),
            encode_max_ms: number_of(in_stats("encode_max_ms")).filter(|n| *n >= 0.0),
            queue_ms: number_of(in_stats("queue_ms")).filter(|n| *n >= 0.0),
            thermal: whole(in_stats("thermal")).filter(|n| (0..=6).contains(n)),
            thermal_headroom: number_of(in_stats("thermal_headroom")),
        };
        let any = reading.wifi_band.is_some() || reading.wifi_rssi.is_some() || reading.battery_temp_c.is_some() || reading.camera_fps.is_some() || reading.encode_ms.is_some();
        any.then_some(reading)
    }

    /// From the camera to the phone sending the frame: encoding and waiting to go out. None until the encoder has been measured.
    pub fn phone_delay_ms(&self) -> Option<f64> {
        self.encode_ms.map(|e| e + self.queue_ms.unwrap_or(0.0))
    }
}

fn ms(v: f64) -> String {
    tr("{n} ms").field("n", if v < 10.0 { number(v, 1, false) } else { number(v, 0, false) })
}

fn fps(v: f64) -> String {
    tr("{n} fps").field("n", number(v, 1, false))
}

fn whole_fps(v: i64) -> String {
    tr("{n} fps").field("n", v)
}

fn thermal_name(status: i64) -> String {
    match status {
        0 => tr("Cool"),
        1 => tr("Light"),
        2 => tr("Moderate"),
        3 => tr("Severe"),
        4 => tr("Critical"),
        _ => tr("Emergency"),
    }
}

/// What a signal in dBm means for a stream.
fn signal_kind(rssi: i64) -> &'static str {
    match rssi {
        i64::MIN..=-75 => "status_warn",
        -74..=-67 => "",
        _ => "status_ok",
    }
}

/// The rows of the Phone section the phone's own state fills. `delay` is the last minute of the phone's delay, one value a second.
pub fn rows(p: &PhoneReading, nerd: bool, delay: &[f32], period_ms: f32) -> Vec<Row> {
    let mut out = Vec::new();
    if let Some(total) = p.phone_delay_ms() {
        let note = match (p.encode_ms, p.queue_ms) {
            (Some(e), Some(q)) => tr("from the camera {e}, waiting to go out {q}").field("e", ms(e)).field("q", ms(q)),
            (Some(e), None) => tr("from the camera {e}").field("e", ms(e)),
            _ => String::new(),
        };
        let slow = total as f32 > period_ms;
        out.push(Row::line(&tr("Delay on the phone"), ms(total)).note(note).kind(if slow { "status_warn" } else { "" }).graph(delay, period_ms).pin(PIN_DELAY));
    } else if p.takes_stats {
        out.push(Row::line(&tr("Delay on the phone"), tr("Measuring")).kind("status_dim"));
    } else {
        out.push(Row::line(&tr("Delay on the phone"), tr("Needs a newer phone app")).kind("status_dim"));
    }
    if let Some(rssi) = p.wifi_rssi {
        let value = match &p.wifi_band {
            Some(band) => tr("{band} GHz, {n} dBm").field("band", band.clone()).field("n", rssi),
            None => tr("{n} dBm").field("n", rssi),
        };
        let note = p.wifi_mbps.map(|m| tr("link speed {n} Mbps").field("n", m)).unwrap_or_default();
        out.push(Row::line(&tr("Phone's Wi-Fi"), value).note(note).kind(signal_kind(rssi)).pin(PIN_WIFI));
    } else if let Some(band) = &p.wifi_band {
        out.push(Row::line(&tr("Phone's Wi-Fi"), tr("{band} GHz").field("band", band.clone())).pin(PIN_WIFI));
    }
    if let Some(t) = p.battery_temp_c {
        out.push(Row::line(&tr("Temperature"), tr("{n} °C").field("n", number(t, 1, false))).pin(PIN_TEMPERATURE));
    }
    match p.thermal {
        Some(status) => {
            // The phone's own cap for heat counts as throttling even while Android's status is still low
            let hot = status >= THROTTLING_FROM;
            let capped = p.heat_fps.is_some();
            let mut note = match p.heat_fps {
                Some(cap) => tr("capped at {n}").field("n", whole_fps(cap)),
                None => String::new(),
            };
            if let (false, false, Some(h)) = (hot, capped, p.thermal_headroom) {
                if h >= 0.9 {
                    note = tr("close to throttling");
                }
            }
            let value = if hot { tr("Yes, {level}").field("level", thermal_name(status).to_lowercase()) } else if capped { tr("Yes") } else { tr("No") };
            out.push(Row::line(&tr("Throttling"), value).kind(if hot || capped { "status_warn" } else { "status_ok" }).note(note).pin(PIN_THROTTLING));
        }
        None if p.heat_fps.is_some() => {
            out.push(Row::line(&tr("Throttling"), tr("Yes")).kind("status_warn").note(tr("capped at {n}").field("n", whole_fps(p.heat_fps.unwrap_or(0)))).pin(PIN_THROTTLING));
        }
        None => {}
    }
    if nerd {
        if let Some(camera) = p.camera_fps {
            let mut note = p.asked_fps.map(|a| tr("asked for {n}").field("n", fps(a))).unwrap_or_default();
            if let Some(cap) = p.dynamic_fps {
                let stepped = tr("lowered to {n} because of the connection").field("n", whole_fps(cap));
                note = if note.is_empty() { stepped } else { format!("{note}, {stepped}") };
            }
            out.push(Row::line(&tr("Camera frame rate"), fps(camera)).note(note).pin(PIN_CAMERA_FPS));
        }
        if let Some(enc) = p.encoder_fps {
            out.push(Row::line(&tr("Encoder frame rate"), fps(enc)).pin(PIN_ENCODER_FPS));
        }
        if let (Some(avg), Some(max)) = (p.encode_ms, p.encode_max_ms) {
            out.push(Row::line(&tr("From the camera"), ms(avg)).note(tr("slowest {n}").field("n", ms(max))).pin(PIN_ENCODE));
        }
        if let Some(q) = p.queue_ms {
            out.push(Row::line(&tr("Send queue"), ms(q)).note(tr("longest a packet waited")).pin(PIN_QUEUE));
        }
        out.push(Row::line(&tr("Round trip, jitter and clock offset"), tr("Not available yet")).kind("status_dim"));
    }
    out
}

/// The same for Copy diagnostics: plain English lines.
pub fn summary(p: &PhoneReading) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut put = |name: &str, value: String| out.push((format!("Stats phone {name}"), value));
    if let Some(d) = p.phone_delay_ms() {
        put("delay", format!("{d:.1} ms on the phone (from the camera {:.1}, waiting to go out {:.1}, slowest {:.1})", p.encode_ms.unwrap_or(0.0), p.queue_ms.unwrap_or(0.0), p.encode_max_ms.unwrap_or(0.0)));
    }
    if p.wifi_band.is_some() || p.wifi_rssi.is_some() {
        put("wifi", format!("{} GHz, {} dBm, {} Mbps", p.wifi_band.as_deref().unwrap_or("?"), p.wifi_rssi.map_or("?".into(), |n| n.to_string()), p.wifi_mbps.map_or("?".into(), |n| n.to_string())));
    }
    if let Some(t) = p.battery_temp_c {
        put("temperature", format!("{t:.1} C"));
    }
    if let Some(s) = p.thermal {
        put("thermal status", format!("{s} ({}){}", thermal_code(s), p.heat_fps.map_or(String::new(), |c| format!(", capped at {c} fps"))));
    }
    if let Some(c) = p.camera_fps {
        put("frame rates", format!("camera {c:.1} fps, asked {}, encoder {}", p.asked_fps.map_or("?".into(), |a| format!("{a:.0}")), p.encoder_fps.map_or("?".into(), |e| format!("{e:.1} fps"))));
    }
    out
}

fn thermal_code(status: i64) -> &'static str {
    ["none", "light", "moderate", "severe", "critical", "emergency", "shutdown"].get(status as usize).copied().unwrap_or("?")
}
