//! The level meter's state (widgets/level_meter.py without the painting): peak and RMS bars that sink, a held peak
//! marker, and clip and limiter lights that latch for a while. The card draws it from these numbers.

use serde_json::{json, Value};

pub const FLOOR_DB: f64 = -60.0;
/// How fast the bars sink once the sound stops.
pub const FALL_DB_PER_S: f64 = 24.0;
/// The peak marker stays put this long, then sinks.
pub const HOLD_S: f64 = 1.5;
pub const CLIP_HOLD_S: f64 = 3.0;
/// Within a couple of steps of full scale counts as clipped.
pub const CLIP_AT: f64 = 0.998;
/// The scale stretches above this, like a DAW's, so the loud end isn't a sliver.
pub const KNEE_DB: f64 = -20.0;
pub const KNEE_AT: f64 = 0.5;

pub fn to_db(level: f64) -> f64 {
    if level > 0.0 { (20.0 * level.log10()).max(FLOOR_DB) } else { FLOOR_DB }
}

/// Where db sits along the bar, 0..1.
pub fn position(db: f64) -> f64 {
    if db <= KNEE_DB {
        KNEE_AT * (db - FLOOR_DB) / (KNEE_DB - FLOOR_DB)
    } else {
        KNEE_AT + (1.0 - KNEE_AT) * (db - KNEE_DB) / -KNEE_DB
    }
}

pub struct LevelMeter {
    clock: Box<dyn Fn() -> f64 + Send>,
    peak_db: f64,
    rms_db: f64,
    hold_db: f64,
    hold_until: f64,
    clip_until: f64,
    limit_until: f64,
    last: Option<f64>,
    pub muted: bool,
}

impl Default for LevelMeter {
    fn default() -> Self {
        LevelMeter::new(Box::new(crate::jitter::monotonic))
    }
}

impl LevelMeter {
    pub fn new(clock: Box<dyn Fn() -> f64 + Send>) -> LevelMeter {
        LevelMeter { clock, peak_db: FLOOR_DB, rms_db: FLOOR_DB, hold_db: FLOOR_DB, hold_until: 0.0, clip_until: 0.0,
                     limit_until: 0.0, last: None, muted: false }
    }

    /// Fed about 30 times a second.
    pub fn set_level(&mut self, peak: f64, rms: f64, limited: bool) {
        let now = (self.clock)();
        let dt = self.last.map_or(0.0, |last| (now - last).min(0.5));
        self.last = Some(now);
        let fall = FALL_DB_PER_S * dt;
        self.peak_db = to_db(peak).max(self.peak_db - fall);
        self.rms_db = to_db(rms).max(self.rms_db - fall);
        if self.peak_db >= self.hold_db {
            self.hold_db = self.peak_db;
            self.hold_until = now + HOLD_S;
        } else if now >= self.hold_until {
            self.hold_db = self.peak_db.max(self.hold_db - fall);
        }
        if peak >= CLIP_AT {
            self.clip_until = now + CLIP_HOLD_S;
        }
        if limited {
            self.limit_until = now + CLIP_HOLD_S;
        }
    }

    /// Back to silence, as when the audio stops.
    pub fn clear(&mut self) {
        self.peak_db = FLOOR_DB;
        self.rms_db = FLOOR_DB;
        self.hold_db = FLOOR_DB;
        self.hold_until = 0.0;
        self.clip_until = 0.0;
        self.limit_until = 0.0;
        self.last = None;
    }

    /// A click clears the held peak and the lights.
    pub fn click(&mut self) {
        self.hold_db = self.peak_db;
        self.hold_until = 0.0;
        self.clip_until = 0.0;
        self.limit_until = 0.0;
    }

    pub fn held_db(&self) -> f64 {
        self.hold_db
    }

    pub fn clipping(&self) -> bool {
        (self.clock)() < self.clip_until
    }

    pub fn limiting(&self) -> bool {
        (self.clock)() < self.limit_until
    }

    /// What the card's LevelMeter draws: bar ends and the marker as 0..1 along it, and which light is on.
    pub fn props(&self) -> Value {
        let lit = if self.muted { "" } else if self.clipping() { "clip" } else if self.limiting() { "limit" } else { "" };
        json!({
            "peak": if self.peak_db > FLOOR_DB { position(self.peak_db) } else { 0.0 },
            "rms": if self.rms_db > FLOOR_DB { position(self.rms_db) } else { 0.0 },
            "hold": if self.hold_db > FLOOR_DB { position(self.hold_db) } else { -1.0 },
            "lit": lit,
            "muted": self.muted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn meter() -> (LevelMeter, Arc<Mutex<f64>>) {
        let t = Arc::new(Mutex::new(100.0));
        let c = t.clone();
        (LevelMeter::new(Box::new(move || *c.lock().unwrap())), t)
    }

    #[test]
    fn to_db_floors_silence() {
        assert!(to_db(0.0) == FLOOR_DB && to_db(1e-9) == FLOOR_DB);
        assert!(to_db(1.0) == 0.0 && (to_db(0.5) + 6.02).abs() < 0.01);
    }

    #[test]
    fn peak_holds_then_sinks() {
        let (mut m, t) = meter();
        m.set_level(0.5, 0.1, false);
        let held = m.held_db();
        for _ in 0..(HOLD_S / 0.1) as usize - 1 {
            *t.lock().unwrap() += 0.1;
            m.set_level(0.01, 0.01, false);
        }
        assert_eq!(m.held_db(), held); // still holding
        for _ in 0..5 {
            *t.lock().unwrap() += 0.1;
            m.set_level(0.01, 0.01, false);
        }
        assert!(m.held_db() < held);
    }

    #[test]
    fn a_louder_peak_moves_the_hold_up_at_once() {
        let (mut m, t) = meter();
        m.set_level(0.1, 0.05, false);
        *t.lock().unwrap() += 0.03;
        m.set_level(0.8, 0.3, false);
        assert!((m.held_db() - to_db(0.8)).abs() < 1e-9);
    }

    #[test]
    fn clip_light_latches_for_a_while_and_a_click_clears_it() {
        let (mut m, t) = meter();
        m.set_level(0.9, 0.3, false);
        assert!(!m.clipping());
        m.set_level(1.0, 0.5, false);
        *t.lock().unwrap() += CLIP_HOLD_S - 0.1;
        m.set_level(0.01, 0.01, false);
        assert!(m.clipping());
        *t.lock().unwrap() += 0.2;
        assert!(!m.clipping());
        m.set_level(1.0, 0.5, false);
        m.click();
        assert!(!m.clipping());
    }

    #[test]
    fn limiting_lights_the_dot_without_counting_as_a_clip() {
        let (mut m, t) = meter();
        m.set_level(0.89, 0.3, true);
        assert!(m.limiting() && !m.clipping());
        assert_eq!(m.props()["lit"], "limit");
        *t.lock().unwrap() += CLIP_HOLD_S + 0.1;
        assert!(!m.limiting());
    }
}
