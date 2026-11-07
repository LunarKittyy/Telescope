//! Holding a source's auto exposure and auto white balance where they are, so a plugin can measure a picture that doesn't
//! drift under it. A phone locks them with Camera2 and says so in its state once the camera has confirmed it; a webcam
//! switches its auto controls off at their current values. This is the bookkeeping the host keeps for either, as plain
//! data: it never touches a phone or a window, the host's glue sends the controls and says what changed on the bus.

use serde_json::{json, Value};
use std::collections::HashMap;

/// What Event::ExposureLock says. "locking" and "locked" follow a lock request; "unlocked" follows an unlock, and also
/// the source letting go by itself (another lens, the camera reopening, the stream stopping).
pub const LOCKING: &str = "locking";
pub const LOCKED: &str = "locked";
pub const UNLOCKED: &str = "unlocked";
/// The source can't lock: a lens or webcam without the controls, a phone app that doesn't know the action, no stream.
pub const UNAVAILABLE: &str = "unavailable";
/// The source took the request and never confirmed it in time, so it's been let go again.
pub const FAILED: &str = "failed";

/// How long a phone gets to confirm a lock before it's let go again.
pub const CONFIRM_WITHIN_S: f64 = 8.0;
/// How often the phone is asked while a lock is still settling, and how often once it holds (to notice it letting go).
pub const POLL_LOCKING_S: f64 = 0.3;
pub const POLL_LOCKED_S: f64 = 3.0;

/// The /v1/control body that asks a phone to lock (or let go).
pub fn phone_body(on: bool) -> Value {
    json!({"action": "exposure_lock", "value": if on { "1" } else { "0" }})
}

/// Whether the camera the phone streams from takes exposure_lock; an older phone app has no such field.
pub fn phone_can_lock(state: &Value) -> bool {
    state.get("cameras").and_then(Value::as_array).is_some_and(|cams| {
        cams.iter().any(|c| c.get("current") == Some(&Value::Bool(true)) && c.get("supportsExposureLock") == Some(&Value::Bool(true)))
    })
}

/// "off", "locking" or "locked", as the phone's state says; a phone that doesn't say is off.
pub fn phone_phase(state: &Value) -> &'static str {
    match state.get("exposure_lock").and_then(Value::as_str) {
        Some("locking") => LOCKING,
        Some("locked") => LOCKED,
        _ => "off",
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    Locking,
    Locked,
}

#[derive(Clone, Debug, PartialEq)]
struct Held {
    phase: Phase,
    since: f64,
    /// When the phone was last asked.
    polled: f64,
    /// Whether the phone's state has shown the lock at all yet: before that an "off" only means the request hasn't arrived.
    seen: bool,
    /// A phone is asked about it as it goes on; a webcam's lock is its own word.
    phone: bool,
}

/// One thing to say on the bus: this source's lock is now `state`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Say {
    pub source_id: String,
    pub state: &'static str,
}

fn say(source_id: &str, state: &'static str) -> Say {
    Say { source_id: source_id.into(), state }
}

/// What the host should do after a poll or a tick.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Due {
    /// Phones to ask for their state now.
    pub poll: Vec<String>,
    /// Phones that never confirmed, which are sent an unlock.
    pub unlock: Vec<String>,
    pub says: Vec<Say>,
}

/// Every source's lock, by source id.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Locks {
    held: HashMap<String, Held>,
    /// Whether each phone's current lens can lock, from the last state it sent.
    lockable: HashMap<String, bool>,
}

impl Locks {
    /// "off", "locking" or "locked".
    pub fn phase(&self, source_id: &str) -> &'static str {
        match self.held.get(source_id).map(|h| h.phase) {
            Some(Phase::Locking) => LOCKING,
            Some(Phase::Locked) => LOCKED,
            None => "off",
        }
    }

    pub fn is_empty(&self) -> bool {
        self.held.is_empty()
    }

    pub fn ids(&self) -> Vec<String> {
        self.held.keys().cloned().collect()
    }

    /// Whether the phone's last state said its lens can lock.
    pub fn phone_lockable(&self, source_id: &str) -> bool {
        self.lockable.get(source_id).copied().unwrap_or(false)
    }

    /// A phone is asked to lock: "locking" now when it can, "unavailable" when its lens (or app) can't. The caller sends
    /// phone_body(true) when this says locking.
    pub fn lock_phone(&mut self, source_id: &str, now: f64) -> Say {
        if !self.phone_lockable(source_id) {
            return say(source_id, UNAVAILABLE);
        }
        if self.held.contains_key(source_id) {
            return say(source_id, self.phase(source_id)); // asked again while it's held: nothing new
        }
        self.held.insert(source_id.into(), Held { phase: Phase::Locking, since: now, polled: now, seen: false, phone: true });
        say(source_id, LOCKING)
    }

    /// A source that answers at once (a webcam) took the lock, or couldn't.
    pub fn lock_at_once(&mut self, source_id: &str, locked: bool, now: f64) -> Say {
        if !locked {
            return say(source_id, UNAVAILABLE);
        }
        self.held.insert(source_id.into(), Held { phase: Phase::Locked, since: now, polled: now, seen: true, phone: false });
        say(source_id, LOCKED)
    }

    /// An unlock was asked for: Some("unlocked") if it was held (or settling). The caller sends the unlock either way.
    pub fn unlock(&mut self, source_id: &str) -> Option<Say> {
        self.held.remove(source_id).map(|_| say(source_id, UNLOCKED))
    }

    /// The source stopped or its lens changed, so whatever it held went with it.
    pub fn gone(&mut self, source_id: &str) -> Option<Say> {
        self.lockable.remove(source_id);
        self.unlock(source_id)
    }

    /// A phone's state, from any poll or the stream's own: what it says about the lock.
    pub fn note_state(&mut self, source_id: &str, state: &Value, now: f64) -> Vec<Say> {
        self.lockable.insert(source_id.into(), phone_can_lock(state));
        let Some(held) = self.held.get_mut(source_id).filter(|h| h.phone) else { return Vec::new() };
        // The lens changed to one that can't lock while it was settling
        if held.phase == Phase::Locking && !phone_can_lock(state) {
            self.held.remove(source_id);
            return vec![say(source_id, UNAVAILABLE)];
        }
        match (held.phase, phone_phase(state)) {
            (Phase::Locking, LOCKED) => {
                held.phase = Phase::Locked;
                held.seen = true;
                held.polled = now;
                vec![say(source_id, LOCKED)]
            }
            (Phase::Locking, LOCKING) => {
                held.seen = true;
                Vec::new()
            }
            // The phone let go by itself (another lens, the camera reopening); an "off" before it ever showed the lock is
            // only the request still on its way
            (Phase::Locking, _) if !held.seen => Vec::new(),
            (_, "off") => {
                self.held.remove(source_id);
                vec![say(source_id, UNLOCKED)]
            }
            _ => Vec::new(),
        }
    }

    /// A tick: which phones to ask, and which locks ran out of time.
    pub fn due(&mut self, now: f64) -> Due {
        let mut due = Due::default();
        let mut ids: Vec<String> = self.held.keys().cloned().collect();
        ids.sort();
        for id in ids {
            let held = self.held.get_mut(&id).expect("listed above");
            if !held.phone {
                continue;
            }
            if held.phase == Phase::Locking && now - held.since > CONFIRM_WITHIN_S {
                self.held.remove(&id);
                due.says.push(say(&id, FAILED));
                due.unlock.push(id);
                continue;
            }
            let every = if held.phase == Phase::Locking { POLL_LOCKING_S } else { POLL_LOCKED_S };
            if now - held.polled >= every {
                held.polled = now;
                due.poll.push(id);
            }
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(lock: &str, lockable: bool) -> Value {
        let mut s = json!({"cameras": [{"id": "0", "current": false, "supportsExposureLock": true}, {"id": "1", "current": true, "supportsExposureLock": lockable}]});
        if lock != "off" {
            s["exposure_lock"] = json!(lock);
        }
        s
    }

    fn locks_with_lockable() -> Locks {
        let mut locks = Locks::default();
        locks.note_state("pixel", &state("off", true), 0.0);
        locks
    }

    #[test]
    fn the_current_lens_decides_whether_a_phone_can_lock() {
        assert!(phone_can_lock(&state("off", true)));
        assert!(!phone_can_lock(&state("off", false)), "another lens that can doesn't count");
        assert!(!phone_can_lock(&json!({"cameras": [{"id": "0", "current": true}]})), "an older phone app");
        assert!(!phone_can_lock(&json!({})));
    }

    #[test]
    fn the_phone_says_off_unless_it_says_otherwise() {
        assert_eq!(phone_phase(&json!({})), "off");
        assert_eq!(phone_phase(&state("locking", true)), LOCKING);
        assert_eq!(phone_phase(&state("locked", true)), LOCKED);
        assert_eq!(phone_phase(&json!({"exposure_lock": 3})), "off");
    }

    #[test]
    fn the_body_is_what_the_phone_takes() {
        assert_eq!(phone_body(true), json!({"action": "exposure_lock", "value": "1"}));
        assert_eq!(phone_body(false), json!({"action": "exposure_lock", "value": "0"}));
    }

    #[test]
    fn a_lens_that_cannot_lock_is_unavailable() {
        let mut locks = Locks::default();
        assert_eq!(locks.lock_phone("pixel", 0.0), say("pixel", UNAVAILABLE), "no state heard yet");
        locks.note_state("pixel", &state("off", false), 0.0);
        assert_eq!(locks.lock_phone("pixel", 0.1), say("pixel", UNAVAILABLE));
        assert_eq!(locks.phase("pixel"), "off");
        assert!(locks.is_empty());
    }

    #[test]
    fn it_is_locked_only_once_the_phone_says_so() {
        let mut locks = locks_with_lockable();
        assert_eq!(locks.lock_phone("pixel", 1.0), say("pixel", LOCKING));
        assert_eq!(locks.phase("pixel"), "locking");
        assert!(locks.note_state("pixel", &state("off", true), 1.1).is_empty(), "the request is still on its way");
        assert!(locks.note_state("pixel", &state("locking", true), 1.4).is_empty());
        assert_eq!(locks.phase("pixel"), "locking");
        assert_eq!(locks.note_state("pixel", &state("locked", true), 1.7), vec![say("pixel", LOCKED)]);
        assert_eq!(locks.phase("pixel"), "locked");
        assert!(locks.note_state("pixel", &state("locked", true), 2.0).is_empty(), "said once");
    }

    #[test]
    fn asking_again_while_held_says_nothing_new() {
        let mut locks = locks_with_lockable();
        locks.lock_phone("pixel", 1.0);
        locks.note_state("pixel", &state("locked", true), 1.5);
        assert_eq!(locks.lock_phone("pixel", 2.0), say("pixel", LOCKED));
    }

    #[test]
    fn unlocking_says_so_once() {
        let mut locks = locks_with_lockable();
        locks.lock_phone("pixel", 1.0);
        locks.note_state("pixel", &state("locked", true), 1.5);
        assert_eq!(locks.unlock("pixel"), Some(say("pixel", UNLOCKED)));
        assert_eq!(locks.unlock("pixel"), None);
        assert_eq!(locks.phase("pixel"), "off");
        assert!(locks.note_state("pixel", &state("locked", true), 1.8).is_empty(), "a late state doesn't bring it back");
    }

    #[test]
    fn a_phone_that_lets_go_by_itself_is_noticed() {
        let mut locks = locks_with_lockable();
        locks.lock_phone("pixel", 1.0);
        locks.note_state("pixel", &state("locked", true), 1.5);
        assert_eq!(locks.note_state("pixel", &state("off", true), 5.0), vec![say("pixel", UNLOCKED)]);
        assert!(locks.is_empty());
    }

    #[test]
    fn a_lock_lost_while_settling_is_noticed_once_the_phone_had_shown_it() {
        let mut locks = locks_with_lockable();
        locks.lock_phone("pixel", 1.0);
        locks.note_state("pixel", &state("locking", true), 1.2);
        assert_eq!(locks.note_state("pixel", &state("off", true), 1.5), vec![say("pixel", UNLOCKED)]);
    }

    #[test]
    fn a_lens_that_stops_being_lockable_while_settling_is_unavailable() {
        let mut locks = locks_with_lockable();
        locks.lock_phone("pixel", 1.0);
        assert_eq!(locks.note_state("pixel", &state("off", false), 1.5), vec![say("pixel", UNAVAILABLE)]);
        assert!(locks.is_empty());
    }

    #[test]
    fn a_stream_that_stops_or_changes_lens_lets_go() {
        let mut locks = locks_with_lockable();
        locks.lock_phone("pixel", 1.0);
        assert_eq!(locks.gone("pixel"), Some(say("pixel", UNLOCKED)));
        assert!(!locks.phone_lockable("pixel"), "it has to be heard from again");
        assert_eq!(locks.gone("pixel"), None);
    }

    #[test]
    fn a_lock_that_is_never_confirmed_fails_and_is_let_go() {
        let mut locks = locks_with_lockable();
        locks.lock_phone("pixel", 1.0);
        assert_eq!(locks.due(1.0 + CONFIRM_WITHIN_S - 0.1).says, vec![]);
        let due = locks.due(1.0 + CONFIRM_WITHIN_S + 0.1);
        assert_eq!(due.says, vec![say("pixel", FAILED)]);
        assert_eq!(due.unlock, ["pixel"]);
        assert_eq!(locks.phase("pixel"), "off");
    }

    #[test]
    fn a_settling_lock_is_asked_often_and_a_held_one_rarely() {
        let mut locks = locks_with_lockable();
        locks.lock_phone("pixel", 0.0);
        assert!(locks.due(0.1).poll.is_empty());
        assert_eq!(locks.due(0.4).poll, ["pixel"]);
        assert!(locks.due(0.5).poll.is_empty());
        assert_eq!(locks.due(0.8).poll, ["pixel"]);
        locks.note_state("pixel", &state("locked", true), 0.9);
        assert!(locks.due(2.0).poll.is_empty());
        assert_eq!(locks.due(4.0).poll, ["pixel"]);
    }

    #[test]
    fn a_webcams_lock_is_its_own_word_and_is_never_polled() {
        let mut locks = Locks::default();
        assert_eq!(locks.lock_at_once("webcam:a", false, 0.0), say("webcam:a", UNAVAILABLE), "a webcam without the controls");
        assert!(locks.is_empty());
        assert_eq!(locks.lock_at_once("webcam:a", true, 0.0), say("webcam:a", LOCKED));
        assert_eq!(locks.phase("webcam:a"), "locked");
        assert_eq!(locks.due(100.0), Due::default());
        assert!(locks.note_state("webcam:a", &json!({}), 100.0).is_empty(), "no phone state says anything about it");
        assert_eq!(locks.phase("webcam:a"), "locked");
        assert_eq!(locks.unlock("webcam:a"), Some(say("webcam:a", UNLOCKED)));
    }

    #[test]
    fn each_source_has_its_own_lock() {
        let mut locks = locks_with_lockable();
        locks.note_state("other", &state("off", true), 0.0);
        locks.lock_phone("pixel", 0.0);
        assert_eq!((locks.phase("pixel"), locks.phase("other")), ("locking", "off"));
    }
}
