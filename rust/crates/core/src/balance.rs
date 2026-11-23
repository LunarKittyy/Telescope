//! Whether the graphics card has room for frames right now. One owner for the whole app, so nothing else reacts to the same slowdown on
//! its own: it hears what a frame's wait for the card cost and what the same work cost the CPU, and every frame step that can run either
//! way asks it where to run. Frames stay on the card while it gives them back in time for the stream's frame rate, since there they cost
//! the CPU almost nothing. They leave when the card makes frames late and the CPU would do better: it keeps time, or is clearly quicker.
//! It leaves quickly, because each late frame is a stutter, and comes back slowly, and more slowly each time coming back didn't last.
//! While frames stay off the card nothing measures it, so now and then a trial job does: as big as a frame, its result thrown away.
//! TELESCOPE_FRAME_STEPS=card or cpu pins it, for comparing.

use crate::time::monotonic_ns;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

const MS: u64 = 1_000_000;
/// The time between frames until a stream says its rate: 30 a second.
const INTERVAL: u64 = 33_333_333;
/// A frame that waited for the card longer than this share of the time between frames is late (the card is only part of a frame's way)...
const LATE_SHARE: (u64, u64) = (3, 4);
/// ...and never under this, however fast the stream.
const LATE_LEAST: u64 = 8 * MS;
/// How many of the last LOOKED_AT frames may be late before frames leave the card.
const LATE_ALLOWED: usize = 3;
const LOOKED_AT: usize = 16;
/// A wait under this share of the late line counts as clear, so a card hovering around the line stays left alone.
const CLEAR_SHARE: (u64, u64) = (2, 3);
/// How often a trial runs while frames are off the card.
const TRIAL_EVERY: u64 = 500 * MS;
/// How long every trial has to come back clear before frames return, at first and at most.
const CLEAR_FOR: u64 = 8_000 * MS;
const CLEAR_FOR_MOST: u64 = 120_000 * MS;
/// What a trial that failed counts as: far from clear.
const TRIAL_FAILED: u64 = u64::MAX;
/// A stay on the card at least this long counts as having worked.
const STAYED: u64 = 45_000 * MS;
/// How many frames the CPU's cost is taken from, and how many it needs before it counts.
const CPU_SEEN: usize = 8;
/// How long what the CPU cost is trusted, at first and at most: it doubles each time trying the CPU again found it no quicker.
const CPU_KEPT: u64 = 60_000 * MS;
const CPU_KEPT_MOST: u64 = 600_000 * MS;
/// When the CPU is late too, the card has to wait this much longer than the CPU takes before frames leave it, since the card's wait costs
/// the CPU nothing; trials within it for NO_WORSE_FOR bring frames back.
const QUICKER: (u64, u64) = (3, 2);
const NO_WORSE_FOR: u64 = 1_500 * MS;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Place {
    Card,
    Cpu,
}

/// Why frames moved.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// Too many frames waited too long for the card.
    CardBusy,
    /// Every trial came back clear for long enough.
    CardClear,
    /// The CPU was late too, and no quicker than the card.
    CpuSlower,
}

/// One move of the frame steps, for the stats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
    pub at_ns: u64,
    pub to: Place,
    pub why: Why,
    /// The wait that moved it, in ns.
    pub wait_ns: u64,
}

/// How many moves are remembered.
const KEPT_MOVES: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pin {
    Free,
    To(Place),
}

/// The middle of some times.
fn median(times: impl Iterator<Item = u64>) -> u64 {
    let mut times: Vec<u64> = times.collect();
    times.sort_unstable();
    times.get(times.len() / 2).copied().unwrap_or(0)
}

/// The decision itself, with time passed in.
struct Watch {
    place: Place,
    /// What each recent frame waited for the card, oldest overwritten first.
    recent: [u64; LOOKED_AT],
    next: usize,
    /// The time between the stream's frames.
    interval: u64,
    /// Since when every trial has been clear, while off the card.
    clear_since: Option<u64>,
    clear_for: u64,
    moved_at: u64,
    next_trial: u64,
    /// One is out, and nothing has come back from it yet.
    trial_out: bool,
    swaps: u32,
    moves: VecDeque<Move>,
    /// What the CPU took for the work of each recent frame off the card, how many of those there are, and when the last came.
    cpu: [u64; LOOKED_AT],
    cpu_seen: usize,
    cpu_at: u64,
    cpu_kept: u64,
    /// Since when every trial has been no slower than the CPU while the CPU is late too.
    no_worse_since: Option<u64>,
}

impl Watch {
    fn new() -> Watch {
        Watch {
            place: Place::Card, recent: [0; LOOKED_AT], next: 0, interval: INTERVAL, clear_since: None, clear_for: CLEAR_FOR, moved_at: 0, next_trial: 0,
            trial_out: false, swaps: 0, moves: VecDeque::new(), cpu: [0; LOOKED_AT], cpu_seen: 0, cpu_at: 0, cpu_kept: CPU_KEPT, no_worse_since: None,
        }
    }

    /// A wait past this makes a frame late.
    fn late(&self) -> u64 {
        (self.interval * LATE_SHARE.0 / LATE_SHARE.1).max(LATE_LEAST)
    }

    fn clear(&self) -> u64 {
        self.late() * CLEAR_SHARE.0 / CLEAR_SHARE.1
    }

    /// What the CPU takes for a frame's work, while that is known and recent enough to go by.
    fn cpu_cost(&self, now: u64) -> Option<u64> {
        (self.cpu_seen >= CPU_SEEN && now.saturating_sub(self.cpu_at) < self.cpu_kept).then(|| median(self.cpu[..self.cpu_seen.min(LOOKED_AT)].iter().copied()))
    }

    /// Whether the CPU would do better than a card that waits this long: it keeps time, or it is late too but clearly quicker.
    fn cpu_better(&self, cpu: u64, wait: u64) -> bool {
        cpu < self.late() || cpu.saturating_mul(QUICKER.0) / QUICKER.1 < wait
    }

    fn note_cpu(&mut self, cost: u64, now: u64) {
        if self.place != Place::Cpu {
            return;
        }
        self.cpu[self.cpu_seen % LOOKED_AT] = cost;
        self.cpu_seen += 1;
        self.cpu_at = now;
    }

    fn note(&mut self, wait: u64, now: u64) -> Option<Place> {
        match self.place {
            Place::Card => {
                self.recent[self.next] = wait;
                self.next = (self.next + 1) % LOOKED_AT;
                let late = self.late();
                if self.recent.iter().filter(|w| **w > late).count() <= LATE_ALLOWED {
                    return None;
                }
                // The CPU was measured not long ago and wouldn't do better than the card does when it is late: frames stay, and cost the CPU nothing
                let known = self.cpu_cost(now);
                if known.is_some_and(|cpu| !self.cpu_better(cpu, median(self.recent.iter().copied().filter(|w| *w > late)))) {
                    return None;
                }
                // Coming back didn't last, so the next time has to wait longer, and a long stay starts over
                self.clear_for = if now.saturating_sub(self.moved_at) < STAYED { (self.clear_for * 2).min(CLEAR_FOR_MOST) } else { CLEAR_FOR };
                if known.is_some() {
                    self.cpu_kept = CPU_KEPT;
                }
                self.recent = [0; LOOKED_AT];
                self.clear_since = None;
                self.no_worse_since = None;
                self.cpu_seen = 0;
                self.next_trial = now + TRIAL_EVERY;
                self.moved(Place::Cpu, now, Why::CardBusy, wait)
            }
            Place::Cpu => {
                // The CPU is late too and the card is about as quick, so the card is better: it costs the CPU nothing
                match self.cpu_cost(now) {
                    Some(cpu) if cpu >= self.late() && !self.cpu_better(cpu, wait) => {
                        if now.saturating_sub(*self.no_worse_since.get_or_insert(now)) >= NO_WORSE_FOR {
                            self.cpu_kept = (self.cpu_kept * 2).min(CPU_KEPT_MOST);
                            return self.moved(Place::Card, now, Why::CpuSlower, wait);
                        }
                    }
                    _ => self.no_worse_since = None,
                }
                if wait > self.clear() {
                    self.clear_since = None;
                    return None;
                }
                if now.saturating_sub(*self.clear_since.get_or_insert(now)) < self.clear_for {
                    return None;
                }
                self.moved(Place::Card, now, Why::CardClear, wait)
            }
        }
    }

    fn moved(&mut self, place: Place, now: u64, why: Why, wait_ns: u64) -> Option<Place> {
        self.place = place;
        self.moved_at = now;
        self.swaps += 1;
        if self.moves.len() == KEPT_MOVES {
            self.moves.pop_front();
        }
        self.moves.push_back(Move { at_ns: now, to: place, why, wait_ns: wait_ns.min(10_000 * MS) });
        Some(place)
    }

    fn trial_due(&mut self, now: u64) -> bool {
        if self.place != Place::Cpu || now < self.next_trial {
            return false;
        }
        self.next_trial = now + TRIAL_EVERY;
        if self.trial_out {
            // Still out when the next was due, so the card isn't clear; a stuck card gets no second one
            self.note(TRIAL_FAILED, now);
            return false;
        }
        self.trial_out = true;
        true
    }
}

/// See the top of this file.
pub struct Balance {
    watch: Mutex<Watch>,
    /// What TELESCOPE_FRAME_STEPS says, which always wins.
    pin: Pin,
    /// What a setting asks for while the environment doesn't pin: RUNTIME_FREE or RUNTIME_CARD.
    runtime: AtomicU8,
    last_wait: AtomicU64,
}

const RUNTIME_FREE: u8 = 0;
const RUNTIME_CARD: u8 = 1;

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
        Balance { watch: Mutex::new(Watch::new()), pin, runtime: AtomicU8::new(RUNTIME_FREE), last_wait: AtomicU64::new(0) }
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

    /// The pin in force: the environment's, else a setting's.
    fn pin_now(&self) -> Pin {
        match self.pin {
            Pin::Free if self.runtime.load(Ordering::Relaxed) == RUNTIME_CARD => Pin::To(Place::Card),
            pin => pin,
        }
    }

    /// Keeps frame steps on the graphics card (a setting, or for this run of the app), or lets them move again. TELESCOPE_FRAME_STEPS still wins.
    pub fn keep_on_card(&self, on: bool) {
        let before = self.runtime.swap(if on { RUNTIME_CARD } else { RUNTIME_FREE }, Ordering::Relaxed);
        if on && before != RUNTIME_CARD {
            // Whatever it had seen about the card is stale once it is held there, and letting go starts from the card
            let mut watch = self.watch.lock().unwrap();
            watch.place = Place::Card;
            watch.recent = [0; LOOKED_AT];
            watch.clear_since = None;
            watch.no_worse_since = None;
            watch.trial_out = false;
        }
    }

    /// Whether a setting keeps frame steps on the card.
    pub fn is_kept_on_card(&self) -> bool {
        self.pin == Pin::Free && self.runtime.load(Ordering::Relaxed) == RUNTIME_CARD
    }

    /// Where frame steps that can run either way should run this frame.
    pub fn place(&self) -> Place {
        match self.pin_now() {
            Pin::To(place) => place,
            Pin::Free => self.watch.lock().unwrap().place,
        }
    }

    /// What a frame or a trial just waited for the card.
    pub fn note_wait(&self, wait_ns: u64) {
        self.last_wait.store(wait_ns, Ordering::Relaxed);
        if self.pin_now() != Pin::Free {
            return;
        }
        let now = monotonic_ns();
        let mut watch = self.watch.lock().unwrap();
        if let Some(place) = watch.note(wait_ns, now) {
            let cpu = watch.cpu_cost(now).map_or(String::from("not measured yet"), |ns| format!("{:.1} ms", ns as f64 / 1e6));
            log::info!("Frame steps moved to the {} after a {:.1} ms wait (late past {:.1} ms; the CPU's cost: {cpu})",
                       if place == Place::Card { "graphics card" } else { "CPU" }, wait_ns as f64 / 1e6, watch.late() as f64 / 1e6);
        }
    }

    /// What the CPU just took for the work a frame off the card would have had done there.
    pub fn note_cpu(&self, cost_ns: u64) {
        if self.pin_now() == Pin::Free {
            self.watch.lock().unwrap().note_cpu(cost_ns, monotonic_ns());
        }
    }

    /// The time between a stream's frames, which says how long a frame may wait for the card before it is late.
    pub fn set_interval(&self, interval_ns: u64) {
        if interval_ns > 0 {
            self.watch.lock().unwrap().interval = interval_ns;
        }
    }

    /// True once for each trial that should run now, which has to end with trial_done. Only one is out at a time.
    pub fn trial_due(&self) -> bool {
        self.pin_now() == Pin::Free && self.watch.lock().unwrap().trial_due(monotonic_ns())
    }

    /// A trial came back: what it waited, or None when it failed.
    pub fn trial_done(&self, wait_ns: Option<u64>) {
        self.watch.lock().unwrap().trial_out = false;
        self.note_wait(wait_ns.unwrap_or(TRIAL_FAILED));
    }

    /// A trial that was stopped before it saw anything: the next one may go, and the card isn't blamed.
    pub fn trial_called_off(&self) {
        self.watch.lock().unwrap().trial_out = false;
    }

    /// What the CPU takes for a frame's work off the card, while that is known and recent, in ns.
    pub fn cpu_cost(&self) -> Option<u64> {
        self.watch.lock().unwrap().cpu_cost(monotonic_ns())
    }

    /// How long a frame may wait for the card before it is late, in ns.
    pub fn late_ns(&self) -> u64 {
        self.watch.lock().unwrap().late()
    }

    /// The newest wait heard, in ns.
    pub fn last_wait(&self) -> u64 {
        self.last_wait.load(Ordering::Relaxed)
    }

    /// How many times frames have moved.
    pub fn swaps(&self) -> u32 {
        self.watch.lock().unwrap().swaps
    }

    /// The latest moves, oldest first, with why.
    pub fn moves(&self) -> Vec<Move> {
        self.watch.lock().unwrap().moves.iter().copied().collect()
    }

    /// Whether TELESCOPE_FRAME_STEPS holds frames in one place.
    pub fn is_pinned(&self) -> bool {
        self.pin != Pin::Free
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
        frames(&mut w, 0, 4, 40 * MS);
        assert_eq!(w.place, Place::Cpu);
    }

    #[test]
    fn frames_come_back_only_after_a_clear_stretch() {
        let mut w = Watch::new();
        let mut t = frames(&mut w, 3_600_000 * MS, 4, 40 * MS) + FRAME;
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
        let mut t = frames(&mut w, 0, 4, 40 * MS);
        for i in 0..100 {
            w.note(if i % 10 == 9 { 20 * MS } else { 2 * MS }, t);
            t += TRIAL_EVERY;
        }
        assert_eq!(w.place, Place::Cpu);
    }

    #[test]
    fn a_return_that_fails_waits_longer_next_time_and_a_stay_clears_that() {
        let mut w = Watch::new();
        let mut t = 3_600_000 * MS;
        let mut waits = Vec::new();
        for _ in 0..6 {
            t = frames(&mut w, t, 4, 40 * MS);
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
        frames(&mut w, t, 4, 40 * MS);
        assert_eq!(w.clear_for, CLEAR_FOR);
    }

    #[test]
    fn trials_come_at_their_pace_and_only_off_the_card() {
        let mut w = Watch::new();
        assert!(!w.trial_due(0));
        let t = frames(&mut w, 0, 4, 40 * MS);
        assert!(!w.trial_due(t));
        assert!(w.trial_due(t + TRIAL_EVERY));
        assert!(!w.trial_due(t + TRIAL_EVERY + MS));
        w.trial_out = false;
        assert!(w.trial_due(t + 2 * TRIAL_EVERY));
    }

    #[test]
    fn a_trial_that_doesnt_finish_is_the_only_one_and_counts_as_failed() {
        let mut w = Watch::new();
        let t = frames(&mut w, 0, 4, 40 * MS);
        assert!(w.trial_due(t + TRIAL_EVERY));
        w.clear_since = Some(t);
        for i in 2..1000 {
            assert!(!w.trial_due(t + i * TRIAL_EVERY), "one at a time");
        }
        assert_eq!((w.clear_since, w.place), (None, Place::Cpu), "the stretch starts over");
        w.trial_out = false;
        assert!(w.trial_due(t + 1000 * TRIAL_EVERY), "the next once it's back");
    }

    #[test]
    fn a_trial_that_comes_back_lets_the_next_one_go_and_a_failed_one_isnt_clear() {
        let b = Balance::free();
        (0..4).for_each(|_| b.note_wait(40 * MS));
        b.watch.lock().unwrap().next_trial = 0;
        assert!(b.trial_due());
        b.trial_done(Some(2 * MS));
        assert!(b.watch.lock().unwrap().clear_since.is_some());
        b.watch.lock().unwrap().next_trial = 0;
        assert!(b.trial_due());
        b.trial_done(None);
        assert_eq!(b.watch.lock().unwrap().clear_since, None);
        b.watch.lock().unwrap().next_trial = 0;
        assert!(b.trial_due());
    }

    #[test]
    fn a_trial_called_off_lets_the_next_one_go_without_blaming_the_card() {
        let b = Balance::free();
        (0..4).for_each(|_| b.note_wait(40 * MS));
        b.watch.lock().unwrap().next_trial = 0;
        assert!(b.trial_due());
        b.trial_done(Some(2 * MS));
        let clear = b.watch.lock().unwrap().clear_since;
        b.watch.lock().unwrap().next_trial = 0;
        assert!(b.trial_due());
        b.trial_called_off();
        assert_eq!(b.watch.lock().unwrap().clear_since, clear);
        b.watch.lock().unwrap().next_trial = 0;
        assert!(b.trial_due());
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

    #[test]
    fn a_setting_keeps_frames_on_the_card_and_letting_go_starts_from_the_card() {
        let b = Balance::free();
        {
            let mut w = b.watch.lock().unwrap();
            frames(&mut w, 0, 100, 40 * MS);
            assert_eq!(w.place, Place::Cpu);
        }
        assert_eq!(b.place(), Place::Cpu);
        b.keep_on_card(true);
        assert_eq!((b.place(), b.is_kept_on_card(), b.is_pinned()), (Place::Card, true, false));
        b.note_wait(100 * MS);
        assert_eq!(b.place(), Place::Card, "nothing moves it while it is held");
        assert!(!b.trial_due());
        b.keep_on_card(false);
        assert_eq!((b.place(), b.is_kept_on_card()), (Place::Card, false));
    }

    #[test]
    fn the_environment_pin_wins_over_a_setting() {
        let b = Balance::pinned_to(Place::Cpu);
        b.keep_on_card(true);
        assert_eq!((b.place(), b.is_kept_on_card()), (Place::Cpu, false));
    }

    /// Frames off the card that each cost the CPU cost, at 30 a second, with a trial of the card waiting wait every TRIAL_EVERY; returns the time after them.
    fn off_card(w: &mut Watch, t: u64, span: u64, cost: u64, wait: u64) -> u64 {
        let mut t = t;
        let end = t + span;
        let mut trial = t + TRIAL_EVERY;
        while t < end && w.place == Place::Cpu {
            w.note_cpu(cost, t);
            if t >= trial {
                w.note(wait, t);
                trial += TRIAL_EVERY;
            }
            t += FRAME;
        }
        t
    }

    #[test]
    fn a_card_that_keeps_time_for_the_frame_rate_stays_in_use() {
        let mut w = Watch::new();
        frames(&mut w, 0, 10_000, 20 * MS);
        assert_eq!((w.place, w.swaps), (Place::Card, 0), "20 ms fits 30 frames a second, so the CPU is saved");
    }

    #[test]
    fn a_faster_stream_has_less_time_for_the_card() {
        let mut w = Watch::new();
        w.interval = 16_666_667;
        frames(&mut w, 0, 8, 20 * MS);
        assert_eq!(w.place, Place::Cpu, "20 ms is late at 60 frames a second");
        let mut slow = Watch::new();
        slow.interval = 1_000 * MS;
        assert_eq!(slow.late(), 750 * MS);
        let mut fast = Watch::new();
        fast.interval = 4 * MS;
        assert_eq!(fast.late(), LATE_LEAST, "never under the least");
    }

    #[test]
    fn frames_stay_off_a_late_card_while_the_cpu_keeps_time() {
        let mut w = Watch::new();
        let t = frames(&mut w, 0, 4, 40 * MS);
        assert_eq!(w.place, Place::Cpu);
        off_card(&mut w, t, 60_000 * MS, 6 * MS, 40 * MS);
        assert_eq!((w.place, w.swaps), (Place::Cpu, 1));
        assert_eq!(w.cpu_cost(t + 60_000 * MS), Some(6 * MS));
    }

    #[test]
    fn frames_go_back_to_the_card_when_the_cpu_is_late_too_and_no_quicker() {
        let mut w = Watch::new();
        let mut t = frames(&mut w, 0, 4, 33 * MS);
        assert_eq!(w.place, Place::Cpu, "not measured yet, so the CPU is tried");
        t = off_card(&mut w, t, 10_000 * MS, 30 * MS, 33 * MS);
        assert_eq!(w.place, Place::Card);
        assert_eq!(w.moves.back().unwrap().why, Why::CpuSlower);
        assert!(t < NO_WORSE_FOR + 2 * TRIAL_EVERY + CPU_SEEN as u64 * FRAME + 10 * FRAME, "back soon, not after a clear stretch");
        // Late again on the card, but the CPU was measured no quicker, so frames stay where they cost it nothing
        t = frames(&mut w, t, 1_000, 33 * MS);
        assert_eq!((w.place, w.swaps), (Place::Card, 2));
        // Until what the CPU cost is too old to trust (twice the first time, since the first try found it no quicker): it is tried again
        assert_eq!(w.cpu_kept, CPU_KEPT * 2);
        t = frames(&mut w, t, 2 * CPU_KEPT / FRAME, 33 * MS);
        assert_eq!(w.place, Place::Cpu);
        off_card(&mut w, t, 10_000 * MS, 30 * MS, 33 * MS);
        assert_eq!((w.place, w.cpu_kept), (Place::Card, CPU_KEPT * 4));
    }

    #[test]
    fn a_card_much_slower_than_a_late_cpu_is_left() {
        let mut w = Watch::new();
        let mut t = frames(&mut w, 0, 4, 100 * MS);
        t = off_card(&mut w, t, 30_000 * MS, 30 * MS, 100 * MS);
        assert_eq!(w.place, Place::Cpu, "a game keeps the card busy: 100 ms there is worse than 30 ms on the CPU");
        // Clear trials still bring frames back, and with the CPU known a busy card is left again
        t = off_card(&mut w, t, 30_000 * MS, 30 * MS, 2 * MS);
        assert_eq!(w.place, Place::Card);
        frames(&mut w, t, 4, 100 * MS);
        assert_eq!(w.place, Place::Cpu);
    }

    #[test]
    fn the_cpu_is_only_measured_off_the_card_and_a_move_off_measures_it_again() {
        let mut w = Watch::new();
        (0..20).for_each(|i| w.note_cpu(MS, i));
        assert_eq!(w.cpu_seen, 0, "on the card the CPU does none of this work");
        let t = frames(&mut w, 0, 4, 40 * MS);
        (0..CPU_SEEN as u64 - 1).for_each(|i| w.note_cpu(9 * MS, t + i));
        assert_eq!(w.cpu_cost(t + 10), None, "too few to go by");
        w.note_cpu(9 * MS, t + 10);
        assert_eq!(w.cpu_cost(t + 10), Some(9 * MS));
        assert_eq!(w.cpu_cost(t + 10 + CPU_KEPT), None, "too old");
    }

    #[test]
    fn the_balance_hears_the_frame_rate_and_the_cpu_only_while_free() {
        let b = Balance::free();
        b.set_interval(16_666_667);
        b.set_interval(0);
        assert_eq!(b.watch.lock().unwrap().interval, 16_666_667, "nothing for a rate it doesn't know");
        (0..4).for_each(|_| b.note_wait(20 * MS));
        assert_eq!(b.place(), Place::Cpu);
        b.note_cpu(5 * MS);
        assert_eq!(b.watch.lock().unwrap().cpu_seen, 1);
        b.keep_on_card(true);
        b.note_cpu(5 * MS);
        assert_eq!(b.watch.lock().unwrap().cpu_seen, 1, "held on the card");
    }
}
