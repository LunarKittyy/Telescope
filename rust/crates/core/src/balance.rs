//! Whether the graphics card has room for frames right now. One owner for the whole app, so nothing else reacts to the same slowdown on
//! its own: it hears what a frame's wait for the card cost and what the same work cost the CPU, and every frame step that can run either
//! way asks it where to run. Frames stay on the card while it gives them back in time for the stream's frame rate, since there they cost
//! the CPU almost nothing. They leave when the card makes frames late and the CPU would do better: it keeps time, or is clearly quicker.
//! Each stream tells it through its own Lane, so a frame is late by its own stream's rate and the CPU's cost is that stream's steps'.
//! It leaves quickly, because each late frame is a stutter, and comes back slowly, and more slowly each time coming back didn't last.
//! While frames stay off the card nothing measures it, so only trials count then (a step that only runs on the card says nothing of the
//! work that left): now and then a job as big as a frame, its result thrown away.
//! TELESCOPE_FRAME_STEPS=card or cpu pins it, for comparing.

use crate::time::monotonic_ns;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
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

/// No trial back for this many trials' time, or this many of the stream's frames, means frames stopped coming (a pause): what was seen before says nothing about now.
const GAP_TRIALS: u64 = 3;

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
    /// The CPU was late too, and not clearly quicker than the card.
    CpuSlower,
}

/// One move of the frame steps, for the stats.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Move {
    pub at_ns: u64,
    pub to: Place,
    pub why: Why,
    /// The wait that moved it, in ns: the middle of the late frames' waits for CardBusy, the trial's for the others.
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

/// What the balance knows of one stream: frames are late by its own rate, and the CPU's cost is what its own steps take.
#[derive(Clone, Copy)]
struct Stream {
    /// The time between the stream's frames.
    interval: u64,
    /// What the CPU took for the work of each recent frame off the card, how many of those there are, and when the last came.
    cpu: [u64; LOOKED_AT],
    cpu_seen: usize,
    cpu_at: u64,
    /// The late line when frames last left the card: a stream that has slowed since is no longer held to the line that moved them.
    late_when_left: u64,
}

impl Stream {
    const fn new(interval: u64) -> Stream {
        let mut s = Stream { interval, cpu: [0; LOOKED_AT], cpu_seen: 0, cpu_at: 0, late_when_left: 0 };
        s.late_when_left = s.late();
        s
    }

    /// A wait past this makes a frame late.
    const fn late(&self) -> u64 {
        let late = self.interval * LATE_SHARE.0 / LATE_SHARE.1;
        if late > LATE_LEAST { late } else { LATE_LEAST }
    }

    /// A trial under this counts as clear: well under the line, or, for a stream that slowed since frames left, under its new line.
    fn clear(&self) -> u64 {
        let clear = self.late() * CLEAR_SHARE.0 / CLEAR_SHARE.1;
        if self.late() > self.late_when_left { self.late() } else { clear }
    }

    /// How long without a trial means frames stopped coming.
    fn gap(&self) -> u64 {
        (GAP_TRIALS * TRIAL_EVERY).max(GAP_TRIALS * self.interval)
    }

    /// Whether the CPU would do better than a card that waits this long: it keeps time, or it is late too but clearly quicker.
    fn cpu_better(&self, cpu: u64, wait: u64) -> bool {
        cpu < self.late() || cpu.saturating_mul(QUICKER.0) / QUICKER.1 < wait
    }
}

/// A stream that never said anything of itself: 30 frames a second, its CPU cost unknown.
const UNKNOWN: Stream = Stream::new(INTERVAL);

/// One frame's wait for the card, judged by its own stream's rate when it came.
#[derive(Clone, Copy, Default)]
struct Seen {
    wait: u64,
    late: bool,
    stream: u64,
}

/// The decision itself, with time passed in.
struct Watch {
    place: Place,
    /// The recent frames' waits for the card, oldest overwritten first.
    recent: [Seen; LOOKED_AT],
    next: usize,
    streams: HashMap<u64, Stream>,
    /// Since when every trial has been clear, while off the card.
    clear_since: Option<u64>,
    clear_for: u64,
    moved_at: u64,
    next_trial: u64,
    /// One is out, and nothing has come back from it yet.
    trial_out: bool,
    /// When the last trial came back.
    trial_back: Option<u64>,
    swaps: u32,
    moves: VecDeque<Move>,
    cpu_kept: u64,
    /// Since when every trial has been no slower than the CPU while the CPU is late too.
    no_worse_since: Option<u64>,
}

impl Watch {
    fn new() -> Watch {
        Watch {
            place: Place::Card, recent: [Seen::default(); LOOKED_AT], next: 0, streams: HashMap::new(), clear_since: None, clear_for: CLEAR_FOR, moved_at: 0,
            next_trial: 0, trial_out: false, trial_back: None, swaps: 0, moves: VecDeque::new(), cpu_kept: CPU_KEPT, no_worse_since: None,
        }
    }

    fn stream(&self, id: u64) -> &Stream {
        self.streams.get(&id).unwrap_or(&UNKNOWN)
    }

    fn stream_mut(&mut self, id: u64) -> &mut Stream {
        self.streams.entry(id).or_insert(UNKNOWN)
    }

    fn set_interval(&mut self, id: u64, interval: u64) {
        self.streams.entry(id).or_insert(Stream::new(interval)).interval = interval;
    }

    /// What the CPU takes for a stream's frame, while that is known and recent enough to go by.
    fn cpu_cost(&self, id: u64, now: u64) -> Option<u64> {
        let s = self.stream(id);
        (s.cpu_seen >= CPU_SEEN && now.saturating_sub(s.cpu_at) < self.cpu_kept).then(|| median(s.cpu[..s.cpu_seen.min(LOOKED_AT)].iter().copied()))
    }

    fn note_cpu(&mut self, id: u64, cost: u64, now: u64) {
        if self.place != Place::Cpu {
            return;
        }
        let s = self.stream_mut(id);
        s.cpu[s.cpu_seen % LOOKED_AT] = cost;
        s.cpu_seen += 1;
        s.cpu_at = now;
    }

    /// What the CPU cost no longer stands for the work: the stream's steps changed, or a setting held frames on the card.
    fn forget_cpu(&mut self, id: Option<u64>) {
        self.streams.iter_mut().filter(|(s, _)| id.is_none_or(|id| id == **s)).for_each(|(_, s)| s.cpu_seen = 0);
    }

    /// A frame's wait for the card. Only frames on the card count: off it, a step that only runs there (its readback) says nothing of the work that left.
    fn note_frame(&mut self, id: u64, wait: u64, now: u64) -> Option<Move> {
        if self.place != Place::Card {
            return None;
        }
        let late = wait > self.stream(id).late();
        self.recent[self.next] = Seen { wait, late, stream: id };
        self.next = (self.next + 1) % LOOKED_AT;
        if !late || self.recent.iter().filter(|s| s.late).count() <= LATE_ALLOWED {
            return None;
        }
        let waited = median(self.recent.iter().filter(|s| s.late && s.stream == id).map(|s| s.wait));
        // The CPU was measured not long ago and wouldn't do better than the card does when it is late: frames stay, and cost the CPU nothing
        let known = self.cpu_cost(id, now);
        if known.is_some_and(|cpu| !self.stream(id).cpu_better(cpu, waited)) {
            return None;
        }
        // Coming back didn't last, so the next time has to wait longer, and a long stay starts over
        self.clear_for = if now.saturating_sub(self.moved_at) < STAYED { (self.clear_for * 2).min(CLEAR_FOR_MOST) } else { CLEAR_FOR };
        if known.is_some() {
            self.cpu_kept = CPU_KEPT;
        }
        self.recent = [Seen::default(); LOOKED_AT];
        self.start_over();
        for s in self.streams.values_mut() {
            s.cpu_seen = 0;
            s.late_when_left = s.late();
        }
        self.next_trial = now + TRIAL_EVERY;
        self.moved(Place::Cpu, now, Why::CardBusy, waited)
    }

    /// What a trial of the card waited, while frames are off it. One that comes back after they returned has nothing left to say.
    fn note_trial(&mut self, id: u64, wait: u64, now: u64) -> Option<Move> {
        if self.place != Place::Cpu {
            return None;
        }
        let s = *self.stream(id);
        if self.trial_back.is_some_and(|back| now.saturating_sub(back) > s.gap()) {
            self.start_over();
        }
        self.trial_back = Some(now);
        // The CPU is late too and the card about as quick, so the card is better: it costs the CPU nothing. This way back waits longer each
        // time coming back didn't last, as the clear one does
        match self.cpu_cost(id, now) {
            Some(cpu) if cpu >= s.late() && !s.cpu_better(cpu, wait) => {
                if now.saturating_sub(*self.no_worse_since.get_or_insert(now)) >= NO_WORSE_FOR * (self.clear_for / CLEAR_FOR) {
                    self.cpu_kept = (self.cpu_kept * 2).min(CPU_KEPT_MOST);
                    return self.moved(Place::Card, now, Why::CpuSlower, wait);
                }
            }
            _ => self.no_worse_since = None,
        }
        if wait > s.clear() {
            self.clear_since = None;
            return None;
        }
        if now.saturating_sub(*self.clear_since.get_or_insert(now)) < self.clear_for {
            return None;
        }
        self.moved(Place::Card, now, Why::CardClear, wait)
    }

    /// The trials' stretches start again.
    fn start_over(&mut self) {
        self.clear_since = None;
        self.no_worse_since = None;
        self.trial_back = None;
    }

    fn moved(&mut self, place: Place, now: u64, why: Why, wait_ns: u64) -> Option<Move> {
        self.place = place;
        self.moved_at = now;
        self.swaps += 1;
        if self.moves.len() == KEPT_MOVES {
            self.moves.pop_front();
        }
        let m = Move { at_ns: now, to: place, why, wait_ns: wait_ns.min(10_000 * MS) };
        self.moves.push_back(m);
        Some(m)
    }

    fn trial_due(&mut self, id: u64, now: u64) -> bool {
        if self.place != Place::Cpu || now < self.next_trial {
            return false;
        }
        self.next_trial = now + TRIAL_EVERY;
        if self.trial_out {
            // Still out when the next was due, so the card isn't clear; a stuck card gets no second one
            self.note_trial(id, TRIAL_FAILED, now);
            return false;
        }
        self.trial_out = true;
        true
    }

    /// A trial came back, in one go so nothing comes between it being in and what it saw.
    fn trial_done(&mut self, id: u64, wait: Option<u64>, now: u64) -> Option<Move> {
        self.trial_out = false;
        self.note_trial(id, wait.unwrap_or(TRIAL_FAILED), now)
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

/// The stream a caller without one of its own counts as (a Spout reader, tests).
const NO_STREAM: u64 = 0;
static NEXT_STREAM: AtomicU64 = AtomicU64::new(1);

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

    /// A stream's own way in: its rate and what its steps cost the CPU are its own, and gone with it.
    pub fn lane(self: &Arc<Self>) -> Lane {
        Lane(Arc::new(LaneOf { balance: self.clone(), id: NEXT_STREAM.fetch_add(1, Ordering::Relaxed) }))
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
            // Whatever it had seen is stale once frames are held on the card, and letting go starts from the card
            let mut watch = self.watch.lock().unwrap();
            watch.place = Place::Card;
            watch.recent = [Seen::default(); LOOKED_AT];
            watch.start_over();
            watch.forget_cpu(None);
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

    /// What a frame just waited for the card, from a caller with no stream of its own.
    pub fn note_wait(&self, wait_ns: u64) {
        self.note_frame(NO_STREAM, wait_ns);
    }

    fn note_frame(&self, id: u64, wait_ns: u64) {
        self.last_wait.store(wait_ns, Ordering::Relaxed);
        if self.pin_now() == Pin::Free {
            let now = monotonic_ns();
            let mut watch = self.watch.lock().unwrap();
            let moved = watch.note_frame(id, wait_ns, now);
            Self::tell(&watch, id, moved, now);
        }
    }

    /// The log line for a move.
    fn tell(watch: &Watch, id: u64, moved: Option<Move>, now: u64) {
        let Some(m) = moved else { return };
        let cpu = watch.cpu_cost(id, now).map_or(String::from("not measured yet"), |ns| format!("{:.1} ms", ns as f64 / 1e6));
        log::info!("Frame steps moved to the {} ({:?}) after a {:.1} ms wait (late past {:.1} ms; the CPU's cost: {cpu})",
                   if m.to == Place::Card { "graphics card" } else { "CPU" }, m.why, m.wait_ns as f64 / 1e6, watch.stream(id).late() as f64 / 1e6);
    }

    fn trial_due_for(&self, id: u64) -> bool {
        self.pin_now() == Pin::Free && self.watch.lock().unwrap().trial_due(id, monotonic_ns())
    }

    fn trial_done_for(&self, id: u64, wait_ns: Option<u64>) {
        self.last_wait.store(wait_ns.unwrap_or(TRIAL_FAILED), Ordering::Relaxed);
        let now = monotonic_ns();
        let mut watch = self.watch.lock().unwrap();
        let moved = if self.pin_now() == Pin::Free { watch.trial_done(id, wait_ns, now) } else { watch.trial_out = false; None };
        Self::tell(&watch, id, moved, now);
    }

    /// True once for each trial that should run now, which has to end with trial_done. Only one is out at a time.
    pub fn trial_due(&self) -> bool {
        self.trial_due_for(NO_STREAM)
    }

    /// A trial came back: what it waited, or None when it failed.
    pub fn trial_done(&self, wait_ns: Option<u64>) {
        self.trial_done_for(NO_STREAM, wait_ns);
    }

    /// A trial that was stopped before it saw anything: the next one may go, and the card isn't blamed.
    pub fn trial_called_off(&self) {
        self.watch.lock().unwrap().trial_out = false;
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

struct LaneOf {
    balance: Arc<Balance>,
    id: u64,
}

impl Drop for LaneOf {
    fn drop(&mut self) {
        self.balance.watch.lock().unwrap_or_else(|e| e.into_inner()).streams.remove(&self.id);
    }
}

/// One stream's way in to the balance (Balance::lane). Clones are the same stream, which is forgotten once the last goes.
#[derive(Clone)]
pub struct Lane(Arc<LaneOf>);

impl Lane {
    pub fn balance(&self) -> &Arc<Balance> {
        &self.0.balance
    }

    /// Where frame steps that can run either way should run this frame.
    pub fn place(&self) -> Place {
        self.0.balance.place()
    }

    /// What one of this stream's frames just waited for the card.
    pub fn note_wait(&self, wait_ns: u64) {
        self.0.balance.note_frame(self.0.id, wait_ns);
    }

    /// What the CPU just took for the work one of this stream's frames off the card would have had done there.
    pub fn note_cpu(&self, cost_ns: u64) {
        if self.0.balance.pin_now() == Pin::Free {
            self.0.balance.watch.lock().unwrap().note_cpu(self.0.id, cost_ns, monotonic_ns());
        }
    }

    /// The time between this stream's frames, which says how long one may wait for the card before it is late.
    pub fn set_interval(&self, interval_ns: u64) {
        if interval_ns > 0 {
            self.0.balance.watch.lock().unwrap().set_interval(self.0.id, interval_ns);
        }
    }

    /// This stream's steps changed, so what they cost the CPU is measured again.
    pub fn steps_changed(&self) {
        self.0.balance.watch.lock().unwrap().forget_cpu(Some(self.0.id));
    }

    /// True once for each trial this stream should run now, which has to end with trial_done. Only one is out at a time, of every stream.
    pub fn trial_due(&self) -> bool {
        self.0.balance.trial_due_for(self.0.id)
    }

    /// This stream's trial came back: what it waited, or None when it failed.
    pub fn trial_done(&self, wait_ns: Option<u64>) {
        self.0.balance.trial_done_for(self.0.id, wait_ns);
    }

    /// A trial that was stopped before it saw anything: the next one may go, and the card isn't blamed.
    pub fn trial_called_off(&self) {
        self.0.balance.trial_called_off();
    }

    /// What the CPU takes for this stream's frame off the card, while that is known and recent, in ns.
    pub fn cpu_cost(&self) -> Option<u64> {
        self.0.balance.watch.lock().unwrap().cpu_cost(self.0.id, monotonic_ns())
    }

    /// How long this stream's frame may wait for the card before it is late, in ns.
    pub fn late_ns(&self) -> u64 {
        self.0.balance.watch.lock().unwrap().stream(self.0.id).late()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: u64 = 33 * MS;
    /// Long after the last move, so the first move off isn't taken for a return that didn't last.
    const LATER: u64 = 3_600_000 * MS;
    const AT_60: u64 = 16_666_667;

    /// Frames of a stream at 30 a second, each waiting wait, from time t; returns the time after them.
    fn frames_of(w: &mut Watch, id: u64, t: u64, count: u64, wait: u64) -> u64 {
        (0..count).fold(t, |t, _| {
            w.note_frame(id, wait, t);
            t + FRAME
        })
    }

    fn frames(w: &mut Watch, t: u64, count: u64, wait: u64) -> u64 {
        frames_of(w, NO_STREAM, t, count, wait)
    }

    fn trial(w: &mut Watch, wait: u64, t: u64) {
        w.note_trial(NO_STREAM, wait, t);
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
        let mut t = frames(&mut w, LATER, 4, 40 * MS) + FRAME;
        for _ in 0..(CLEAR_FOR / TRIAL_EVERY) {
            trial(&mut w, 2 * MS, t);
            t += TRIAL_EVERY;
        }
        assert_eq!(w.place, Place::Cpu, "not quite long enough");
        trial(&mut w, 2 * MS, t);
        assert_eq!(w.place, Place::Card);
    }

    #[test]
    fn one_slow_trial_starts_the_stretch_over() {
        let mut w = Watch::new();
        let mut t = frames(&mut w, 0, 4, 40 * MS);
        for i in 0..100 {
            trial(&mut w, if i % 10 == 9 { 20 * MS } else { 2 * MS }, t);
            t += TRIAL_EVERY;
        }
        assert_eq!(w.place, Place::Cpu);
    }

    #[test]
    fn a_return_that_fails_waits_longer_next_time_and_a_stay_clears_that() {
        let mut w = Watch::new();
        let mut t = LATER;
        let mut waits = Vec::new();
        for _ in 0..6 {
            t = frames(&mut w, t, 4, 40 * MS);
            assert_eq!(w.place, Place::Cpu);
            waits.push(w.clear_for);
            t += FRAME;
            let began = t;
            while w.place == Place::Cpu {
                trial(&mut w, 2 * MS, t);
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
        assert!(!w.trial_due(NO_STREAM, 0));
        let t = frames(&mut w, 0, 4, 40 * MS);
        assert!(!w.trial_due(NO_STREAM, t));
        assert!(w.trial_due(NO_STREAM, t + TRIAL_EVERY));
        assert!(!w.trial_due(NO_STREAM, t + TRIAL_EVERY + MS));
        w.trial_out = false;
        assert!(w.trial_due(NO_STREAM, t + 2 * TRIAL_EVERY));
    }

    #[test]
    fn a_trial_that_doesnt_finish_is_the_only_one_and_counts_as_failed() {
        let mut w = Watch::new();
        let t = frames(&mut w, 0, 4, 40 * MS);
        assert!(w.trial_due(NO_STREAM, t + TRIAL_EVERY));
        w.clear_since = Some(t);
        for i in 2..1000 {
            assert!(!w.trial_due(NO_STREAM, t + i * TRIAL_EVERY), "one at a time");
        }
        assert_eq!((w.clear_since, w.place), (None, Place::Cpu), "the stretch starts over");
        w.trial_out = false;
        assert!(w.trial_due(NO_STREAM, t + 1000 * TRIAL_EVERY), "the next once it's back");
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
    fn a_trial_back_after_frames_returned_isnt_taken_for_a_frame() {
        let b = Balance::free();
        (0..4).for_each(|_| b.note_wait(40 * MS));
        b.watch.lock().unwrap().next_trial = 0;
        assert!(b.trial_due());
        // Frames went back to the card while this trial was still on its way
        b.watch.lock().unwrap().place = Place::Card;
        b.trial_done(Some(50 * MS));
        let w = b.watch.lock().unwrap();
        assert_eq!((w.place, w.trial_out), (Place::Card, false));
        assert!(w.recent.iter().all(|s| !s.late), "the trial's wait isn't one of the card's frames");
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
        let mut next = t + TRIAL_EVERY;
        while t < end && w.place == Place::Cpu {
            w.note_cpu(NO_STREAM, cost, t);
            if t >= next {
                trial(w, wait, t);
                next += TRIAL_EVERY;
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
        w.set_interval(1, AT_60);
        frames_of(&mut w, 1, 0, 8, 20 * MS);
        assert_eq!(w.place, Place::Cpu, "20 ms is late at 60 frames a second");
        assert_eq!(Stream::new(1_000 * MS).late(), 750 * MS);
        assert_eq!(Stream::new(4 * MS).late(), LATE_LEAST, "never under the least");
    }

    #[test]
    fn each_stream_is_late_by_its_own_rate() {
        let mut w = Watch::new();
        w.set_interval(1, INTERVAL);
        w.set_interval(2, AT_60);
        let mut t = 0;
        for _ in 0..100 {
            w.note_frame(1, 20 * MS, t);
            w.note_frame(2, 2 * MS, t);
            w.note_frame(3, 20 * MS, t);
            t += FRAME;
        }
        assert_eq!(w.place, Place::Card, "20 ms keeps time at 30 a second, and a stream of no known rate isn't held to another's");
        frames_of(&mut w, 2, t, 4, 20 * MS);
        assert_eq!(w.place, Place::Cpu, "but not at 60");
    }

    #[test]
    fn a_frame_is_late_by_the_rate_it_came_at_and_a_move_says_what_late_frames_waited() {
        let mut w = Watch::new();
        w.set_interval(1, INTERVAL);
        let t = frames_of(&mut w, 1, 0, LOOKED_AT as u64, 20 * MS);
        w.set_interval(1, AT_60);
        w.note_frame(1, 2 * MS, t);
        assert_eq!(w.place, Place::Card, "the frames before kept time for the rate they came at");
        for (i, wait) in [20, 22, 24, 26].into_iter().enumerate() {
            w.note_frame(1, wait * MS, t + (i as u64 + 1) * FRAME);
        }
        assert_eq!(w.place, Place::Cpu);
        assert_eq!(w.moves.back().unwrap().wait_ns, 24 * MS, "the middle of the late frames, not the last one");
    }

    #[test]
    fn frames_stay_off_a_late_card_while_the_cpu_keeps_time() {
        let mut w = Watch::new();
        let t = frames(&mut w, 0, 4, 40 * MS);
        assert_eq!(w.place, Place::Cpu);
        off_card(&mut w, t, 60_000 * MS, 6 * MS, 40 * MS);
        assert_eq!((w.place, w.swaps), (Place::Cpu, 1));
        assert_eq!(w.cpu_cost(NO_STREAM, t + 60_000 * MS), Some(6 * MS));
    }

    #[test]
    fn frames_go_back_to_the_card_when_the_cpu_is_late_too_and_no_quicker() {
        let mut w = Watch::new();
        let mut t = frames(&mut w, LATER, 4, 33 * MS);
        assert_eq!(w.place, Place::Cpu, "not measured yet, so the CPU is tried");
        let left = t;
        t = off_card(&mut w, t, 10_000 * MS, 30 * MS, 33 * MS);
        assert_eq!(w.place, Place::Card);
        assert_eq!(w.moves.back().unwrap().why, Why::CpuSlower);
        assert!(t - left < NO_WORSE_FOR + 2 * TRIAL_EVERY + CPU_SEEN as u64 * FRAME + 10 * FRAME, "back soon, not after a clear stretch");
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
    fn coming_back_because_the_cpu_is_late_too_waits_longer_each_time_it_didnt_last() {
        // Trials are lighter than a whole frame's work, so they look quick while the frames themselves are late on the card
        let mut w = Watch::new();
        let (mut t, mut next, mut left) = (LATER, 0, 0);
        let mut stays = Vec::new();
        while t < LATER + 120_000 * MS {
            if w.place == Place::Card {
                w.note_frame(NO_STREAM, 50 * MS, t);
                (left, next) = (t, t + TRIAL_EVERY);
            } else {
                w.note_cpu(NO_STREAM, 30 * MS, t);
                if t >= next {
                    trial(&mut w, 10 * MS, t);
                    next += TRIAL_EVERY;
                    if w.place == Place::Card {
                        stays.push(t - left);
                    }
                }
            }
            t += FRAME;
        }
        assert!(stays.len() >= 3, "{stays:?}");
        for (i, stay) in stays.iter().enumerate() {
            let most = CLEAR_FOR_MOST / CLEAR_FOR;
            assert!(*stay >= NO_WORSE_FOR * (1 << i).min(most), "stay {i} of {stays:?}");
        }
    }

    #[test]
    fn a_step_that_only_runs_on_the_card_doesnt_hold_frames_off_it() {
        let mut w = Watch::new();
        let mut t = frames(&mut w, LATER, 4, 40 * MS);
        let left = t;
        let mut next = t + TRIAL_EVERY;
        while w.place == Place::Cpu && t < left + 60_000 * MS {
            // Its readback is still a frame's wait for the card, but not of the work that left it
            w.note_frame(NO_STREAM, 50 * MS, t);
            if t >= next {
                trial(&mut w, 2 * MS, t);
                next += TRIAL_EVERY;
            }
            t += FRAME;
        }
        assert_eq!(w.place, Place::Card);
        assert!(t - left <= CLEAR_FOR + 3 * TRIAL_EVERY, "{} ms {:?}", (t - left) / MS, w.moves);
    }

    #[test]
    fn a_stream_that_slows_down_is_held_to_its_new_rate_and_a_card_near_the_line_isnt_brought_back() {
        let mut w = Watch::new();
        w.set_interval(1, AT_60);
        let mut t = frames_of(&mut w, 1, LATER, 4, 20 * MS);
        assert_eq!(w.place, Place::Cpu);
        w.set_interval(1, INTERVAL);
        let left = t;
        while w.place == Place::Cpu && t < left + 60_000 * MS {
            w.note_cpu(1, 6 * MS, t);
            w.note_trial(1, 20 * MS, t);
            t += TRIAL_EVERY;
        }
        assert_eq!(w.place, Place::Card, "20 ms keeps time at 30 a second");
        assert!(t - left <= CLEAR_FOR + 2 * TRIAL_EVERY);
        // A stream that stayed at 30 a second and left a card waiting 30 ms isn't brought back by one waiting 20, near its line
        let mut w = Watch::new();
        let t = frames(&mut w, LATER, 4, 30 * MS);
        off_card(&mut w, t, 60_000 * MS, 6 * MS, 20 * MS);
        assert_eq!(w.place, Place::Cpu);
    }

    #[test]
    fn what_the_cpu_cost_is_forgotten_when_the_steps_change() {
        let mut w = Watch::new();
        let mut t = frames_of(&mut w, 1, LATER, 4, 60 * MS);
        while w.place == Place::Cpu {
            w.note_cpu(1, 55 * MS, t);
            w.note_trial(1, 2 * MS, t);
            t += TRIAL_EVERY;
        }
        t = frames_of(&mut w, 1, t, 100, 40 * MS);
        assert_eq!(w.place, Place::Card, "55 ms on the CPU is no better than 40 ms on the card");
        w.forget_cpu(Some(1));
        assert_eq!(w.cpu_cost(1, t), None);
        frames_of(&mut w, 1, t, 4, 40 * MS);
        assert_eq!(w.place, Place::Cpu, "other steps, so the CPU is tried again");
    }

    #[test]
    fn a_pause_starts_the_trials_stretches_over() {
        let mut w = Watch::new();
        let t = frames(&mut w, LATER, 4, 40 * MS);
        (0..CPU_SEEN as u64).for_each(|i| w.note_cpu(NO_STREAM, 30 * MS, t + i));
        trial(&mut w, 35 * MS, t + TRIAL_EVERY);
        let resumed = t + 10_000 * MS;
        trial(&mut w, 35 * MS, resumed);
        assert_eq!(w.place, Place::Cpu, "the time paused says nothing of the card");
        let mut t = resumed;
        while w.place == Place::Cpu {
            t += TRIAL_EVERY;
            trial(&mut w, 35 * MS, t);
        }
        assert_eq!(t - resumed, NO_WORSE_FOR);
    }

    #[test]
    fn the_cpu_is_only_measured_off_the_card_and_a_move_off_measures_it_again() {
        let mut w = Watch::new();
        (0..20).for_each(|i| w.note_cpu(NO_STREAM, MS, i));
        assert_eq!(w.cpu_cost(NO_STREAM, 20), None, "on the card the CPU does none of this work");
        let t = frames(&mut w, 0, 4, 40 * MS);
        (0..CPU_SEEN as u64 - 1).for_each(|i| w.note_cpu(NO_STREAM, 9 * MS, t + i));
        assert_eq!(w.cpu_cost(NO_STREAM, t + 10), None, "too few to go by");
        w.note_cpu(NO_STREAM, 9 * MS, t + 10);
        assert_eq!(w.cpu_cost(NO_STREAM, t + 10), Some(9 * MS));
        assert_eq!(w.cpu_cost(NO_STREAM, t + 10 + CPU_KEPT), None, "too old");
    }

    #[test]
    fn a_lane_tells_its_own_rate_and_cost_only_while_free_and_is_forgotten_with_its_stream() {
        let b = Arc::new(Balance::free());
        let lane = b.lane();
        lane.set_interval(AT_60);
        lane.set_interval(0);
        assert_eq!(lane.late_ns(), AT_60 * 3 / 4, "nothing for a rate it doesn't know");
        (0..4).for_each(|_| lane.note_wait(20 * MS));
        assert_eq!(lane.place(), Place::Cpu);
        (0..CPU_SEEN).for_each(|_| lane.note_cpu(5 * MS));
        assert_eq!(lane.cpu_cost(), Some(5 * MS));
        lane.steps_changed();
        assert_eq!(lane.cpu_cost(), None, "other steps cost something else");
        (0..CPU_SEEN).for_each(|_| lane.note_cpu(5 * MS));
        b.keep_on_card(true);
        assert_eq!(lane.cpu_cost(), None, "held on the card, what it cost before is stale");
        lane.note_cpu(5 * MS);
        assert_eq!(b.watch.lock().unwrap().stream(lane.0.id).cpu_seen, 0, "and nothing is measured");
        b.keep_on_card(false);
        let other = lane.clone();
        drop(lane);
        assert_eq!(b.watch.lock().unwrap().streams.len(), 1, "a clone is the same stream");
        drop(other);
        assert!(b.watch.lock().unwrap().streams.is_empty());
    }
}
