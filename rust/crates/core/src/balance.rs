//! Whether the graphics card has room for frames right now. One owner for the whole app, so nothing else reacts to the same slowdown on
//! its own: it hears what a frame's wait for the card cost, and every frame step that can run either way asks it where to run. It leaves
//! the card quickly, because each late frame is a stutter, and comes back slowly, and more slowly each time coming back didn't last.
//! While frames stay off the card nothing measures it, so now and then a trial job does: as big as a frame, its result thrown away.
//! TELESCOPE_FRAME_STEPS=card or cpu pins it, for comparing.

use crate::time::monotonic_ns;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

const MS: u64 = 1_000_000;
/// A frame that waited longer than this is late. The stream has about 33 ms a frame, and the card is only part of that.
const LATE: u64 = 8 * MS;
/// How many of the last LOOKED_AT frames may be late before frames leave the card.
const LATE_ALLOWED: usize = 3;
const LOOKED_AT: usize = 16;
/// Well under LATE, so a card hovering around it stays left alone.
const CLEAR: u64 = 3 * MS;
/// How often a trial runs while frames are off the card.
const TRIAL_EVERY: u64 = 500 * MS;
/// How long every trial has to come back clear before frames return, at first and at most.
const CLEAR_FOR: u64 = 8_000 * MS;
const CLEAR_FOR_MOST: u64 = 120_000 * MS;
/// A stay on the card at least this long counts as having worked.
const STAYED: u64 = 45_000 * MS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Card,
    Cpu,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pin {
    Free,
    To(Place),
}

/// The decision itself, with time passed in.
struct Watch {
    place: Place,
    /// Whether each recent frame was late, oldest overwritten first.
    recent: [bool; LOOKED_AT],
    next: usize,
    /// Since when every trial has been clear, while off the card.
    clear_since: Option<u64>,
    clear_for: u64,
    moved_at: u64,
    next_trial: u64,
    swaps: u32,
}

impl Watch {
    fn new() -> Watch {
        Watch { place: Place::Card, recent: [false; LOOKED_AT], next: 0, clear_since: None, clear_for: CLEAR_FOR, moved_at: 0, next_trial: 0, swaps: 0 }
    }

    fn note(&mut self, wait: u64, now: u64) -> Option<Place> {
        match self.place {
            Place::Card => {
                self.recent[self.next] = wait > LATE;
                self.next = (self.next + 1) % LOOKED_AT;
                if self.recent.iter().filter(|late| **late).count() <= LATE_ALLOWED {
                    return None;
                }
                // Coming back didn't last, so the next time has to wait longer, and a long stay starts over
                self.clear_for = if now.saturating_sub(self.moved_at) < STAYED { (self.clear_for * 2).min(CLEAR_FOR_MOST) } else { CLEAR_FOR };
                self.recent = [false; LOOKED_AT];
                self.clear_since = None;
                self.next_trial = now + TRIAL_EVERY;
                self.moved(Place::Cpu, now)
            }
            Place::Cpu => {
                if wait > CLEAR {
                    self.clear_since = None;
                    return None;
                }
                if now.saturating_sub(*self.clear_since.get_or_insert(now)) < self.clear_for {
                    return None;
                }
                self.moved(Place::Card, now)
            }
        }
    }

    fn moved(&mut self, place: Place, now: u64) -> Option<Place> {
        self.place = place;
        self.moved_at = now;
        self.swaps += 1;
        Some(place)
    }

    fn trial_due(&mut self, now: u64) -> bool {
        let due = self.place == Place::Cpu && now >= self.next_trial;
        if due {
            self.next_trial = now + TRIAL_EVERY;
        }
        due
    }
}

/// See the top of this file.
pub struct Balance {
    watch: Mutex<Watch>,
    pin: Pin,
    last_wait: AtomicU64,
}

static SHARED: LazyLock<Arc<Balance>> = LazyLock::new(|| {
    let pin = match std::env::var("TELESCOPE_FRAME_STEPS").as_deref() {
        Ok("card") => Pin::To(Place::Card),
        Ok("cpu") => Pin::To(Place::Cpu),
        _ => Pin::Free,
    };
    Arc::new(Balance::pinned(pin))
});

impl Balance {
    fn pinned(pin: Pin) -> Balance {
        Balance { watch: Mutex::new(Watch::new()), pin, last_wait: AtomicU64::new(0) }
    }

    /// The app's one.
    pub fn shared() -> Arc<Balance> {
        SHARED.clone()
    }

    /// One of its own, or pinned to place, for tests.
    pub fn free() -> Balance {
        Balance::pinned(Pin::Free)
    }

    pub fn pinned_to(place: Place) -> Balance {
        Balance::pinned(Pin::To(place))
    }

    /// Where frame steps that can run either way should run this frame.
    pub fn place(&self) -> Place {
        match self.pin {
            Pin::To(place) => place,
            Pin::Free => self.watch.lock().unwrap().place,
        }
    }

    /// What a frame or a trial just waited for the card.
    pub fn note_wait(&self, wait_ns: u64) {
        self.last_wait.store(wait_ns, Ordering::Relaxed);
        if self.pin != Pin::Free {
            return;
        }
        if let Some(place) = self.watch.lock().unwrap().note(wait_ns, monotonic_ns()) {
            log::info!("Frame steps moved to the {} after a {:.1} ms wait", if place == Place::Card { "graphics card" } else { "CPU" }, wait_ns as f64 / 1e6);
        }
    }

    /// True once for each trial that should run now.
    pub fn trial_due(&self) -> bool {
        self.pin == Pin::Free && self.watch.lock().unwrap().trial_due(monotonic_ns())
    }

    /// The newest wait heard, in ns.
    pub fn last_wait(&self) -> u64 {
        self.last_wait.load(Ordering::Relaxed)
    }

    /// How many times frames have moved.
    pub fn swaps(&self) -> u32 {
        self.watch.lock().unwrap().swaps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: u64 = 33 * MS;

    /// Frames at 30 a second, each waiting wait, from time t; returns the time after them.
    fn frames(w: &mut Watch, t: u64, count: u64, wait: u64) -> u64 {
        (0..count).fold(t, |t, _| {
            w.note(wait, t);
            t + FRAME
        })
    }

    #[test]
    fn a_quick_card_stays_in_use() {
        let mut w = Watch::new();
        frames(&mut w, 0, 10_000, 3 * MS);
        assert_eq!((w.place, w.swaps), (Place::Card, 0));
    }

    #[test]
    fn a_hiccup_is_not_a_busy_card() {
        let mut w = Watch::new();
        let mut t = 0;
        for _ in 0..200 {
            t = frames(&mut w, t, 3, 40 * MS);
            t = frames(&mut w, t, 40, 3 * MS);
        }
        assert_eq!(w.place, Place::Card);
    }

    #[test]
    fn a_busy_card_is_left_within_a_few_frames() {
        let mut w = Watch::new();
        frames(&mut w, 0, 4, 25 * MS);
        assert_eq!(w.place, Place::Cpu);
    }

    #[test]
    fn frames_come_back_only_after_a_clear_stretch() {
        let mut w = Watch::new();
        let mut t = frames(&mut w, 3600_000 * MS, 4, 25 * MS) + FRAME;
        for _ in 0..(CLEAR_FOR / TRIAL_EVERY) {
            w.note(2 * MS, t);
            t += TRIAL_EVERY;
        }
        assert_eq!(w.place, Place::Cpu, "not quite long enough");
        w.note(2 * MS, t);
        assert_eq!(w.place, Place::Card);
    }

    #[test]
    fn one_slow_trial_starts_the_stretch_over() {
        let mut w = Watch::new();
        let mut t = frames(&mut w, 0, 4, 25 * MS);
        for i in 0..100 {
            w.note(if i % 10 == 9 { 7 * MS } else { 2 * MS }, t);
            t += TRIAL_EVERY;
        }
        assert_eq!(w.place, Place::Cpu);
    }

    #[test]
    fn a_return_that_fails_waits_longer_next_time_and_a_stay_clears_that() {
        let mut w = Watch::new();
        let mut t = 3600_000 * MS;
        let mut waits = Vec::new();
        for _ in 0..6 {
            t = frames(&mut w, t, 4, 25 * MS);
            assert_eq!(w.place, Place::Cpu);
            waits.push(w.clear_for);
            t += FRAME;
            let began = t;
            while w.place == Place::Cpu {
                w.note(2 * MS, t);
                t += TRIAL_EVERY;
            }
            assert!(t - began >= *waits.last().unwrap());
        }
        assert_eq!(waits[..2], [CLEAR_FOR, CLEAR_FOR * 2]);
        assert_eq!(*waits.last().unwrap(), CLEAR_FOR_MOST);
        t = frames(&mut w, t, STAYED / FRAME + 1, 3 * MS);
        frames(&mut w, t, 4, 25 * MS);
        assert_eq!(w.clear_for, CLEAR_FOR);
    }

    #[test]
    fn trials_come_at_their_pace_and_only_off_the_card() {
        let mut w = Watch::new();
        assert!(!w.trial_due(0));
        let t = frames(&mut w, 0, 4, 25 * MS);
        assert!(!w.trial_due(t));
        assert!(w.trial_due(t + TRIAL_EVERY));
        assert!(!w.trial_due(t + TRIAL_EVERY + MS));
        assert!(w.trial_due(t + 2 * TRIAL_EVERY));
    }

    #[test]
    fn a_pinned_one_never_moves() {
        let b = Balance::pinned_to(Place::Card);
        (0..100).for_each(|_| b.note_wait(90 * MS));
        assert_eq!(b.place(), Place::Card);
        assert!(!b.trial_due());
        assert_eq!(Balance::pinned_to(Place::Cpu).place(), Place::Cpu);
        assert_eq!(b.last_wait(), 90 * MS);
    }
}
