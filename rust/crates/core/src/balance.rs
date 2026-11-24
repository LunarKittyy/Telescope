//! Whether the graphics card has room for a stream's frames right now. Each stream (a Lane) decides for itself where the frame steps that
//! can run either way run: it hears what its frames waited for the card and what the same work cost the CPU. Frames stay on the card
//! while it gives them back in time for the stream's frame rate, since there they cost the CPU almost nothing. They leave when the card
//! makes frames late and the CPU would do better: it keeps time, or is clearly quicker. They leave quickly, because each late frame is a
//! stutter, and come back slowly, and more slowly each time coming back didn't last. While frames stay off the card only trials measure
//! it (a step that only runs on the card says nothing of the work that left): now and then a job as big as a frame, its result thrown
//! away. One Balance for the whole app keeps the lanes, the setting that holds them all on the card and every move, for the stats.
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

/// One move of a stream's frame steps, for the stats.
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

/// One frame's wait for the card, and whether it was late by the stream's rate when it came.
#[derive(Clone, Copy, Default)]
struct Seen {
    wait: u64,
    late: bool,
}

/// One stream's decision, with time passed in.
struct Watch {
    place: Place,
    /// The time between the stream's frames.
    interval: u64,
    /// The recent frames' waits for the card, oldest overwritten first.
    recent: [Seen; LOOKED_AT],
    next: usize,
    /// What the CPU took for the work of each recent frame off the card, how many of those there are, and when the last came.
    cpu: [u64; LOOKED_AT],
    cpu_seen: usize,
    cpu_at: u64,
    cpu_kept: u64,
    /// The late line when frames last left the card: a stream that has slowed since is no longer held to the line that moved them.
    late_when_left: u64,
    /// Since when every trial has been clear, and since when every one has been no slower than a CPU that is late too, while off the card.
    clear_since: Option<u64>,
    no_worse_since: Option<u64>,
    /// When the last trial came back.
    trial_back: Option<u64>,
    clear_for: u64,
    /// How long frames have been on the card since they last came back, counting only while they came (a pause or a lost link isn't a stay), and when the last came.
    stayed: u64,
    frame_at: Option<u64>,
    next_trial: u64,
    /// The trial out, and whether it may say what it saw (not one sent before a setting held frames on the card); how many were sent.
    trial: Option<(u64, bool)>,
    trials: u64,
}

impl Watch {
    fn new(interval: u64) -> Watch {
        let mut w = Watch {
            place: Place::Card, interval, recent: [Seen::default(); LOOKED_AT], next: 0, cpu: [0; LOOKED_AT], cpu_seen: 0, cpu_at: 0, cpu_kept: CPU_KEPT,
            late_when_left: 0, clear_since: None, no_worse_since: None, trial_back: None, clear_for: CLEAR_FOR, stayed: STAYED, frame_at: None, next_trial: 0, trial: None, trials: 0,
        };
        w.late_when_left = w.late();
        w
    }

    /// A wait past this makes a frame late.
    fn late(&self) -> u64 {
        (self.interval * LATE_SHARE.0 / LATE_SHARE.1).max(LATE_LEAST)
    }

    /// A trial under this counts as clear: well under the line, or, for a stream that slowed since frames left, under its new line.
    fn clear(&self) -> u64 {
        if self.late() > self.late_when_left { self.late() } else { self.late() * CLEAR_SHARE.0 / CLEAR_SHARE.1 }
    }

    /// How long without a trial means frames stopped coming.
    fn gap(&self) -> u64 {
        (GAP_TRIALS * TRIAL_EVERY).max(GAP_TRIALS * self.interval)
    }

    /// Whether the CPU would do better than a card that waits this long: it keeps time, or it is late too but clearly quicker.
    fn cpu_better(&self, cpu: u64, wait: u64) -> bool {
        cpu < self.late() || cpu.saturating_mul(QUICKER.0) / QUICKER.1 < wait
    }

    /// What the CPU takes for a frame's work, while that is known and recent enough to go by.
    fn cpu_cost(&self, now: u64) -> Option<u64> {
        (self.cpu_seen >= CPU_SEEN && now.saturating_sub(self.cpu_at) < self.cpu_kept).then(|| median(self.cpu[..self.cpu_seen.min(LOOKED_AT)].iter().copied()))
    }

    fn note_cpu(&mut self, cost: u64, now: u64) {
        if self.place != Place::Cpu {
            return;
        }
        self.cpu[self.cpu_seen % LOOKED_AT] = cost;
        self.cpu_seen += 1;
        self.cpu_at = now;
    }

    /// A frame's wait for the card. Only frames on the card count: off it, a step that only runs there (its readback) says nothing of the work that left.
    fn note_frame(&mut self, wait: u64, now: u64) -> Option<Move> {
        if self.place != Place::Card {
            return None;
        }
        if let Some(at) = self.frame_at.filter(|at| now.saturating_sub(*at) <= self.gap()) {
            self.stayed += now - at;
        }
        self.frame_at = Some(now);
        let late = wait > self.late();
        self.recent[self.next] = Seen { wait, late };
        self.next = (self.next + 1) % LOOKED_AT;
        if !late || self.recent.iter().filter(|s| s.late).count() <= LATE_ALLOWED {
            return None;
        }
        let waited = median(self.recent.iter().filter(|s| s.late).map(|s| s.wait));
        // The CPU was measured not long ago and wouldn't do better than the card does when it is late: frames stay, and cost the CPU nothing
        let known = self.cpu_cost(now);
        if known.is_some_and(|cpu| !self.cpu_better(cpu, waited)) {
            return None;
        }
        // Coming back didn't last, so the next time has to wait longer, and a long stay starts over
        self.clear_for = if self.stayed < STAYED { (self.clear_for * 2).min(CLEAR_FOR_MOST) } else { CLEAR_FOR };
        if known.is_some() {
            self.cpu_kept = CPU_KEPT;
        }
        self.recent = [Seen::default(); LOOKED_AT];
        self.start_over();
        self.late_when_left = self.late();
        self.next_trial = now + TRIAL_EVERY;
        let m = self.moved(Place::Cpu, now, Why::CardBusy, waited, known);
        self.cpu_seen = 0;
        m
    }

    /// What a trial of the card waited, while frames are off it.
    fn note_trial(&mut self, wait: u64, now: u64) -> Option<Move> {
        if self.place != Place::Cpu {
            return None;
        }
        if self.trial_back.is_some_and(|back| now.saturating_sub(back) > self.gap()) {
            self.start_over();
        }
        self.trial_back = Some(now);
        // The CPU is late too and the card about as quick, so the card is better: it costs the CPU nothing. This way back waits longer each
        // time coming back didn't last, as the clear one does
        let known = self.cpu_cost(now);
        match known {
            Some(cpu) if cpu >= self.late() && !self.cpu_better(cpu, wait) => {
                if now.saturating_sub(*self.no_worse_since.get_or_insert(now)) >= NO_WORSE_FOR * (self.clear_for / CLEAR_FOR) {
                    self.cpu_kept = (self.cpu_kept * 2).min(CPU_KEPT_MOST);
                    return self.moved(Place::Card, now, Why::CpuSlower, wait, known);
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
        self.moved(Place::Card, now, Why::CardClear, wait, known)
    }

    /// The trials' stretches start again.
    fn start_over(&mut self) {
        self.clear_since = None;
        self.no_worse_since = None;
        self.trial_back = None;
    }

    /// Frames come again after a pause: what was seen before it says nothing of the time it lasted.
    fn resumed(&mut self) {
        self.start_over();
        self.frame_at = None;
    }

    /// Back on the card and nothing kept from before, for a setting that holds frames there.
    fn held(&mut self) {
        self.place = Place::Card;
        self.recent = [Seen::default(); LOOKED_AT];
        self.start_over();
        self.cpu_seen = 0;
        // One still out stays out, so the next isn't sent while it runs, but what it saw was before the setting
        if let Some((_, counts)) = &mut self.trial {
            *counts = false;
        }
    }

    fn moved(&mut self, place: Place, now: u64, why: Why, wait_ns: u64, cpu: Option<u64>) -> Option<Move> {
        self.place = place;
        (self.stayed, self.frame_at) = (0, None);
        let cpu = cpu.map_or(String::from("not measured yet"), |ns| format!("{:.1} ms", ns as f64 / 1e6));
        log::info!("A stream's frame steps moved to the {} ({why:?}) after a {:.1} ms wait (late past {:.1} ms; the CPU's cost: {cpu})",
                   if place == Place::Card { "graphics card" } else { "CPU" }, wait_ns as f64 / 1e6, self.late() as f64 / 1e6);
        Some(Move { at_ns: now, to: place, why, wait_ns: wait_ns.min(10_000 * MS) })
    }

    /// The ticket of a trial that should run now, which has to end with trial_done. Only one is out at a time.
    fn trial_due(&mut self, now: u64) -> (Option<u64>, Option<Move>) {
        if self.place != Place::Cpu || now < self.next_trial {
            return (None, None);
        }
        self.next_trial = now + TRIAL_EVERY;
        match self.trial {
            // Still out when the next was due, so the card isn't clear; a stuck card gets no second one
            Some((_, true)) => return (None, self.note_trial(TRIAL_FAILED, now)),
            Some((_, false)) => return (None, None),
            None => {}
        }
        self.trials += 1;
        self.trial = Some((self.trials, true));
        (Some(self.trials), None)
    }

    /// A trial came back, in one go so nothing comes between it being in and what it saw. One that isn't the trial out (sent before a
    /// setting held frames on the card) has nothing to say.
    fn trial_done(&mut self, ticket: u64, wait: Option<u64>, now: u64) -> Option<Move> {
        match self.trial {
            Some((out, counts)) if out == ticket => {
                self.trial = None;
                counts.then(|| self.note_trial(wait.unwrap_or(TRIAL_FAILED), now)).flatten()
            }
            _ => None,
        }
    }

    fn trial_called_off(&mut self, ticket: u64) {
        if self.trial.is_some_and(|(out, _)| out == ticket) {
            self.trial = None;
        }
    }
}

/// Every stream's decision, and what the stats show of them.
struct Lanes {
    watches: HashMap<u64, Watch>,
    swaps: u32,
    moves: VecDeque<Move>,
}

impl Lanes {
    fn watch(&mut self, id: u64) -> &mut Watch {
        self.watches.entry(id).or_insert_with(|| Watch::new(INTERVAL))
    }

    /// Runs f on a stream's decision and keeps the move it made.
    fn on<T>(&mut self, id: u64, f: impl FnOnce(&mut Watch) -> (T, Option<Move>)) -> T {
        let (out, moved) = f(self.watch(id));
        if let Some(m) = moved {
            self.swaps += 1;
            if self.moves.len() == KEPT_MOVES {
                self.moves.pop_front();
            }
            self.moves.push_back(m);
        }
        out
    }
}

/// See the top of this file.
pub struct Balance {
    lanes: Mutex<Lanes>,
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
        Balance { lanes: Mutex::new(Lanes { watches: HashMap::new(), swaps: 0, moves: VecDeque::new() }), pin, runtime: AtomicU8::new(RUNTIME_FREE), last_wait: AtomicU64::new(0) }
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

    /// A new stream's own way in, starting on the card: where its steps run, its rate and what they cost the CPU are its own, and gone with it.
    pub fn lane(self: &Arc<Self>) -> Lane {
        let id = NEXT_STREAM.fetch_add(1, Ordering::Relaxed);
        self.lanes.lock().unwrap_or_else(|e| e.into_inner()).watch(id);
        Lane(Arc::new(LaneOf { balance: self.clone(), id }))
    }

    /// The pin in force: the environment's, else a setting's.
    fn pin_now(&self) -> Pin {
        match self.pin {
            Pin::Free if self.runtime.load(Ordering::Relaxed) == RUNTIME_CARD => Pin::To(Place::Card),
            pin => pin,
        }
    }

    fn free_now(&self) -> bool {
        self.pin_now() == Pin::Free
    }

    /// Keeps frame steps on the graphics card (a setting, or for this run of the app), or lets them move again. TELESCOPE_FRAME_STEPS still wins.
    pub fn keep_on_card(&self, on: bool) {
        let before = self.runtime.swap(if on { RUNTIME_CARD } else { RUNTIME_FREE }, Ordering::Relaxed);
        if on && before != RUNTIME_CARD {
            // Whatever was seen is stale once frames are held on the card, and letting go starts from the card
            self.lanes.lock().unwrap_or_else(|e| e.into_inner()).watches.values_mut().for_each(Watch::held);
        }
    }

    /// Whether a setting keeps frame steps on the card.
    pub fn is_kept_on_card(&self) -> bool {
        self.pin == Pin::Free && self.runtime.load(Ordering::Relaxed) == RUNTIME_CARD
    }

    fn place_of(&self, id: u64) -> Place {
        match self.pin_now() {
            Pin::To(place) => place,
            Pin::Free => self.lanes.lock().unwrap_or_else(|e| e.into_inner()).watch(id).place,
        }
    }

    /// Where frame steps that can run either way should run this frame, for a caller with no stream of its own.
    pub fn place(&self) -> Place {
        self.place_of(NO_STREAM)
    }

    /// Whether every stream's steps run on the card now.
    pub fn all_on_card(&self) -> bool {
        match self.pin_now() {
            Pin::To(place) => place == Place::Card,
            Pin::Free => self.lanes.lock().unwrap_or_else(|e| e.into_inner()).watches.values().all(|w| w.place == Place::Card),
        }
    }

    fn note_frame(&self, id: u64, wait_ns: u64) {
        self.last_wait.store(wait_ns, Ordering::Relaxed);
        if self.free_now() {
            let now = monotonic_ns();
            self.lanes.lock().unwrap_or_else(|e| e.into_inner()).on(id, |w| ((), w.note_frame(wait_ns, now)));
        }
    }

    fn trial_due_for(&self, id: u64) -> Option<u64> {
        if !self.free_now() {
            return None;
        }
        let now = monotonic_ns();
        self.lanes.lock().unwrap_or_else(|e| e.into_inner()).on(id, |w| w.trial_due(now))
    }

    fn trial_done_for(&self, id: u64, ticket: u64, wait_ns: Option<u64>) {
        self.last_wait.store(wait_ns.unwrap_or(TRIAL_FAILED), Ordering::Relaxed);
        let now = monotonic_ns();
        let free = self.free_now();
        self.lanes.lock().unwrap_or_else(|e| e.into_inner()).on(id, |w| {
            if free {
                ((), w.trial_done(ticket, wait_ns, now))
            } else {
                w.trial_called_off(ticket);
                ((), None)
            }
        });
    }

    /// What a frame just waited for the card, from a caller with no stream of its own.
    pub fn note_wait(&self, wait_ns: u64) {
        self.note_frame(NO_STREAM, wait_ns);
    }

    /// True once for each trial that should run now for a caller with no stream of its own, which has to end with trial_done. Only one is out at a time.
    pub fn trial_due(&self) -> bool {
        self.trial_due_for(NO_STREAM).is_some()
    }

    /// That trial came back: what it waited, or None when it failed.
    pub fn trial_done(&self, wait_ns: Option<u64>) {
        let ticket = self.lanes.lock().unwrap_or_else(|e| e.into_inner()).watch(NO_STREAM).trial.map(|(ticket, _)| ticket);
        if let Some(ticket) = ticket {
            self.trial_done_for(NO_STREAM, ticket, wait_ns);
        }
    }

    /// That trial was stopped before it saw anything: the next one may go, and the card isn't blamed.
    pub fn trial_called_off(&self) {
        self.lanes.lock().unwrap_or_else(|e| e.into_inner()).watch(NO_STREAM).trial = None;
    }

    /// The newest wait heard, in ns.
    pub fn last_wait(&self) -> u64 {
        self.last_wait.load(Ordering::Relaxed)
    }

    /// How many times a stream's frames have moved.
    pub fn swaps(&self) -> u32 {
        self.lanes.lock().unwrap_or_else(|e| e.into_inner()).swaps
    }

    /// The latest moves of every stream, oldest first, with why.
    pub fn moves(&self) -> Vec<Move> {
        self.lanes.lock().unwrap_or_else(|e| e.into_inner()).moves.iter().copied().collect()
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
        self.balance.lanes.lock().unwrap_or_else(|e| e.into_inner()).watches.remove(&self.id);
    }
}

/// One stream's way in to the balance (Balance::lane). Clones are the same stream, which is forgotten once the last goes.
#[derive(Clone)]
pub struct Lane(Arc<LaneOf>);

impl Lane {
    fn with<T>(&self, f: impl FnOnce(&mut Watch) -> T) -> T {
        f(self.0.balance.lanes.lock().unwrap_or_else(|e| e.into_inner()).watch(self.0.id))
    }

    /// Where this stream's frame steps that can run either way should run this frame.
    pub fn place(&self) -> Place {
        self.0.balance.place_of(self.0.id)
    }

    /// What one of this stream's frames just waited for the card.
    pub fn note_wait(&self, wait_ns: u64) {
        self.0.balance.note_frame(self.0.id, wait_ns);
    }

    /// What the CPU just took for the work one of this stream's frames off the card would have had done there.
    pub fn note_cpu(&self, cost_ns: u64) {
        if self.0.balance.free_now() {
            self.with(|w| w.note_cpu(cost_ns, monotonic_ns()));
        }
    }

    /// The time between this stream's frames, which says how long one may wait for the card before it is late.
    pub fn set_interval(&self, interval_ns: u64) {
        if interval_ns > 0 {
            self.with(|w| w.interval = interval_ns);
        }
    }

    /// This stream's work changed (its steps, the picture's size), so what it costs the CPU is measured again.
    pub fn work_changed(&self) {
        self.with(|w| w.cpu_seen = 0);
    }

    /// Frames come again after a pause.
    pub fn resumed(&self) {
        self.with(Watch::resumed);
    }

    /// The ticket of a trial this stream should run now, which has to end with trial_done or trial_called_off. Only one is out at a time.
    pub fn trial_due(&self) -> Option<u64> {
        self.0.balance.trial_due_for(self.0.id)
    }

    /// This stream's trial came back: what it waited, or None when it failed.
    pub fn trial_done(&self, ticket: u64, wait_ns: Option<u64>) {
        self.0.balance.trial_done_for(self.0.id, ticket, wait_ns);
    }

    /// A trial that was stopped before it saw anything: the next one may go, and the card isn't blamed.
    pub fn trial_called_off(&self, ticket: u64) {
        self.with(|w| w.trial_called_off(ticket));
    }

    /// What the CPU takes for this stream's frame off the card, while that is known and recent, in ns.
    pub fn cpu_cost(&self) -> Option<u64> {
        self.with(|w| w.cpu_cost(monotonic_ns()))
    }

    /// How long this stream's frame may wait for the card before it is late, in ns.
    pub fn late_ns(&self) -> u64 {
        self.with(|w| w.late())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: u64 = 33 * MS;
    /// Long after the last move, so the first move off isn't taken for a return that didn't last.
    const LATER: u64 = 3_600_000 * MS;
    const AT_60: u64 = 16_666_667;

    /// One stream's decision and the moves it made, with time in the test's hands.
    struct Sim {
        w: Watch,
        moves: Vec<Move>,
    }

    impl Sim {
        fn new() -> Sim {
            Sim::at(INTERVAL)
        }

        fn at(interval: u64) -> Sim {
            Sim { w: Watch::new(interval), moves: Vec::new() }
        }

        fn place(&self) -> Place {
            self.w.place
        }

        /// Frames at 30 a second, each waiting wait, from time t; returns the time after them.
        fn frames(&mut self, t: u64, count: u64, wait: u64) -> u64 {
            (0..count).fold(t, |t, _| {
                self.frame(wait, t);
                t + FRAME
            })
        }

        fn frame(&mut self, wait: u64, t: u64) {
            self.moves.extend(self.w.note_frame(wait, t));
        }

        fn trial(&mut self, wait: u64, t: u64) {
            self.moves.extend(self.w.note_trial(wait, t));
        }

        /// Frames off the card that each cost the CPU cost, at 30 a second, with a trial of the card waiting wait every TRIAL_EVERY; returns the time after them.
        fn off_card(&mut self, t: u64, span: u64, cost: u64, wait: u64) -> u64 {
            let mut t = t;
            let end = t + span;
            let mut next = t + TRIAL_EVERY;
            while t < end && self.place() == Place::Cpu {
                self.w.note_cpu(cost, t);
                if t >= next {
                    self.trial(wait, t);
                    next += TRIAL_EVERY;
                }
                t += FRAME;
            }
            t
        }
    }

    #[test]
    fn a_quick_card_stays_in_use() {
        let mut s = Sim::new();
        s.frames(0, 10_000, 3 * MS);
        assert_eq!((s.place(), s.moves.len()), (Place::Card, 0));
    }

    #[test]
    fn a_hiccup_is_not_a_busy_card() {
        let mut s = Sim::new();
        let mut t = 0;
        for _ in 0..200 {
            t = s.frames(t, 3, 40 * MS);
            t = s.frames(t, 40, 3 * MS);
        }
        assert_eq!(s.place(), Place::Card);
    }

    #[test]
    fn a_busy_card_is_left_within_a_few_frames() {
        let mut s = Sim::new();
        s.frames(0, 4, 40 * MS);
        assert_eq!(s.place(), Place::Cpu);
    }

    #[test]
    fn frames_come_back_only_after_a_clear_stretch() {
        let mut s = Sim::new();
        let mut t = s.frames(LATER, 4, 40 * MS) + FRAME;
        for _ in 0..(CLEAR_FOR / TRIAL_EVERY) {
            s.trial(2 * MS, t);
            t += TRIAL_EVERY;
        }
        assert_eq!(s.place(), Place::Cpu, "not quite long enough");
        s.trial(2 * MS, t);
        assert_eq!(s.place(), Place::Card);
    }

    #[test]
    fn one_slow_trial_starts_the_stretch_over() {
        let mut s = Sim::new();
        let mut t = s.frames(0, 4, 40 * MS);
        for i in 0..100 {
            s.trial(if i % 10 == 9 { 20 * MS } else { 2 * MS }, t);
            t += TRIAL_EVERY;
        }
        assert_eq!(s.place(), Place::Cpu);
    }

    #[test]
    fn a_return_that_fails_waits_longer_next_time_and_a_stay_clears_that() {
        let mut s = Sim::new();
        let mut t = LATER;
        let mut waits = Vec::new();
        for _ in 0..6 {
            t = s.frames(t, 4, 40 * MS);
            assert_eq!(s.place(), Place::Cpu);
            waits.push(s.w.clear_for);
            t += FRAME;
            let began = t;
            while s.place() == Place::Cpu {
                s.trial(2 * MS, t);
                t += TRIAL_EVERY;
            }
            assert!(t - began >= *waits.last().unwrap());
        }
        assert_eq!(waits[..2], [CLEAR_FOR, CLEAR_FOR * 2]);
        assert_eq!(*waits.last().unwrap(), CLEAR_FOR_MOST);
        t = s.frames(t, STAYED / FRAME + 1, 3 * MS);
        s.frames(t, 4, 40 * MS);
        assert_eq!(s.w.clear_for, CLEAR_FOR);
    }

    #[test]
    fn trials_come_at_their_pace_and_only_off_the_card() {
        let mut s = Sim::new();
        assert_eq!(s.w.trial_due(0).0, None);
        let t = s.frames(0, 4, 40 * MS);
        assert_eq!(s.w.trial_due(t).0, None);
        let ticket = s.w.trial_due(t + TRIAL_EVERY).0.unwrap();
        assert_eq!(s.w.trial_due(t + TRIAL_EVERY + MS).0, None);
        s.w.trial_done(ticket, Some(2 * MS), t + TRIAL_EVERY + MS);
        assert!(s.w.trial_due(t + 2 * TRIAL_EVERY).0.is_some());
    }

    #[test]
    fn a_trial_that_doesnt_finish_is_the_only_one_and_counts_as_failed() {
        let mut s = Sim::new();
        let t = s.frames(0, 4, 40 * MS);
        let ticket = s.w.trial_due(t + TRIAL_EVERY).0.unwrap();
        s.w.clear_since = Some(t);
        for i in 2..1000 {
            assert_eq!(s.w.trial_due(t + i * TRIAL_EVERY).0, None, "one at a time");
        }
        assert_eq!((s.w.clear_since, s.place()), (None, Place::Cpu), "the stretch starts over");
        s.w.trial_called_off(ticket);
        assert!(s.w.trial_due(t + 1000 * TRIAL_EVERY).0.is_some(), "the next once it's back");
    }

    #[test]
    fn a_trial_that_comes_back_lets_the_next_one_go_and_a_failed_one_isnt_clear() {
        let b = Balance::free();
        (0..4).for_each(|_| b.note_wait(40 * MS));
        let due = |b: &Balance| {
            b.lanes.lock().unwrap_or_else(|e| e.into_inner()).watch(NO_STREAM).next_trial = 0;
            b.trial_due()
        };
        assert!(due(&b));
        b.trial_done(Some(2 * MS));
        assert!(b.lanes.lock().unwrap_or_else(|e| e.into_inner()).watch(NO_STREAM).clear_since.is_some());
        assert!(due(&b));
        b.trial_done(None);
        assert_eq!(b.lanes.lock().unwrap_or_else(|e| e.into_inner()).watch(NO_STREAM).clear_since, None);
        assert!(due(&b));
        b.trial_called_off();
        assert!(due(&b), "called off lets the next go");
    }

    #[test]
    fn a_trial_back_after_frames_returned_isnt_taken_for_a_frame() {
        let mut s = Sim::new();
        let t = s.frames(0, 4, 40 * MS);
        let ticket = s.w.trial_due(t + TRIAL_EVERY).0.unwrap();
        // Frames went back to the card while this trial was still on its way
        s.w.place = Place::Card;
        s.w.trial_done(ticket, Some(50 * MS), t + TRIAL_EVERY + MS);
        assert_eq!((s.place(), s.w.trial), (Place::Card, None));
        assert!(s.w.recent.iter().all(|s| !s.late), "the trial's wait isn't one of the card's frames");
    }

    #[test]
    fn a_trial_sent_before_the_setting_held_frames_says_nothing_after_it_let_go() {
        let b = Arc::new(Balance::free());
        let lane = b.lane();
        (0..4).for_each(|_| lane.note_wait(40 * MS));
        lane.with(|w| w.next_trial = 0);
        let ticket = lane.trial_due().unwrap();
        b.keep_on_card(true);
        b.keep_on_card(false);
        (0..4).for_each(|_| lane.note_wait(40 * MS));
        assert_eq!(lane.place(), Place::Cpu);
        lane.with(|w| w.next_trial = 0);
        assert_eq!(lane.trial_due(), None, "the old one is still out, so a new one doesn't wait on it");
        lane.trial_done(ticket, Some(2 * MS));
        assert_eq!(lane.with(|w| w.clear_since), None, "and what it saw was before the setting");
        lane.with(|w| w.next_trial = 0);
        assert!(lane.trial_due().is_some());
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
        let b = Arc::new(Balance::free());
        let lane = b.lane();
        (0..100).for_each(|_| lane.note_wait(40 * MS));
        assert_eq!((lane.place(), b.all_on_card()), (Place::Cpu, false));
        b.keep_on_card(true);
        assert_eq!((lane.place(), b.is_kept_on_card(), b.is_pinned(), b.all_on_card()), (Place::Card, true, false, true));
        lane.note_wait(100 * MS);
        assert_eq!(lane.place(), Place::Card, "nothing moves it while it is held");
        assert_eq!(lane.trial_due(), None);
        b.keep_on_card(false);
        assert_eq!((lane.place(), b.is_kept_on_card()), (Place::Card, false));
    }

    #[test]
    fn the_environment_pin_wins_over_a_setting() {
        let b = Arc::new(Balance::pinned_to(Place::Cpu));
        b.keep_on_card(true);
        assert_eq!((b.lane().place(), b.is_kept_on_card(), b.all_on_card()), (Place::Cpu, false, false));
    }

    #[test]
    fn each_stream_decides_where_its_own_steps_run() {
        let b = Arc::new(Balance::free());
        let (slow, quick) = (b.lane(), b.lane());
        quick.set_interval(AT_60);
        for _ in 0..100 {
            slow.note_wait(2 * MS);
            quick.note_wait(20 * MS);
        }
        assert_eq!((slow.place(), quick.place(), b.all_on_card()), (Place::Card, Place::Cpu, false), "20 ms is late at 60 a second only");
        assert_eq!(slow.trial_due(), None, "and only the stream off the card tries it");
        // One that comes after starts on the card, with nothing of a stream that has gone
        drop(quick);
        assert!(b.all_on_card());
        let next = b.lane();
        next.note_wait(30 * MS);
        assert_eq!(next.place(), Place::Card);
        assert_eq!(b.moves().len(), 1);
    }

    #[test]
    fn a_card_that_keeps_time_for_the_frame_rate_stays_in_use() {
        let mut s = Sim::new();
        s.frames(0, 10_000, 20 * MS);
        assert_eq!((s.place(), s.moves.len()), (Place::Card, 0), "20 ms fits 30 frames a second, so the CPU is saved");
    }

    #[test]
    fn a_faster_stream_has_less_time_for_the_card() {
        let mut s = Sim::at(AT_60);
        s.frames(0, 8, 20 * MS);
        assert_eq!(s.place(), Place::Cpu, "20 ms is late at 60 frames a second");
        assert_eq!(Watch::new(1_000 * MS).late(), 750 * MS);
        assert_eq!(Watch::new(4 * MS).late(), LATE_LEAST, "never under the least");
    }

    #[test]
    fn a_frame_is_late_by_the_rate_it_came_at_and_a_move_says_what_late_frames_waited() {
        let mut s = Sim::new();
        let t = s.frames(0, LOOKED_AT as u64, 20 * MS);
        s.w.interval = AT_60;
        s.frame(2 * MS, t);
        assert_eq!(s.place(), Place::Card, "the frames before kept time for the rate they came at");
        for (i, wait) in [20, 22, 24, 26].into_iter().enumerate() {
            s.frame(wait * MS, t + (i as u64 + 1) * FRAME);
        }
        assert_eq!(s.place(), Place::Cpu);
        assert_eq!(s.moves.last().unwrap().wait_ns, 24 * MS, "the middle of the late frames, not the last one");
    }

    #[test]
    fn frames_stay_off_a_late_card_while_the_cpu_keeps_time() {
        let mut s = Sim::new();
        let t = s.frames(0, 4, 40 * MS);
        assert_eq!(s.place(), Place::Cpu);
        s.off_card(t, 60_000 * MS, 6 * MS, 40 * MS);
        assert_eq!((s.place(), s.moves.len()), (Place::Cpu, 1));
        assert_eq!(s.w.cpu_cost(t + 60_000 * MS), Some(6 * MS));
    }

    #[test]
    fn frames_go_back_to_the_card_when_the_cpu_is_late_too_and_no_quicker() {
        let mut s = Sim::new();
        let mut t = s.frames(LATER, 4, 33 * MS);
        assert_eq!(s.place(), Place::Cpu, "not measured yet, so the CPU is tried");
        let left = t;
        t = s.off_card(t, 10_000 * MS, 30 * MS, 33 * MS);
        assert_eq!(s.place(), Place::Card);
        assert_eq!(s.moves.last().unwrap().why, Why::CpuSlower);
        assert!(t - left < NO_WORSE_FOR + 2 * TRIAL_EVERY + CPU_SEEN as u64 * FRAME + 10 * FRAME, "back soon, not after a clear stretch");
        // Late again on the card, but the CPU was measured no quicker, so frames stay where they cost it nothing
        t = s.frames(t, 1_000, 33 * MS);
        assert_eq!((s.place(), s.moves.len()), (Place::Card, 2));
        // Until what the CPU cost is too old to trust (twice the first time, since the first try found it no quicker): it is tried again
        assert_eq!(s.w.cpu_kept, CPU_KEPT * 2);
        t = s.frames(t, 2 * CPU_KEPT / FRAME, 33 * MS);
        assert_eq!(s.place(), Place::Cpu);
        s.off_card(t, 10_000 * MS, 30 * MS, 33 * MS);
        assert_eq!((s.place(), s.w.cpu_kept), (Place::Card, CPU_KEPT * 4));
    }

    #[test]
    fn coming_back_because_the_cpu_is_late_too_waits_longer_each_time_it_didnt_last() {
        // Trials are lighter than a whole frame's work, so they look quick while the frames themselves are late on the card
        let mut s = Sim::new();
        let (mut t, mut next, mut left) = (LATER, 0, 0);
        let mut stays = Vec::new();
        while t < LATER + 120_000 * MS {
            if s.place() == Place::Card {
                s.frame(50 * MS, t);
                (left, next) = (t, t + TRIAL_EVERY);
            } else {
                s.w.note_cpu(30 * MS, t);
                if t >= next {
                    s.trial(10 * MS, t);
                    next += TRIAL_EVERY;
                    if s.place() == Place::Card {
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
        let mut s = Sim::new();
        let mut t = s.frames(LATER, 4, 40 * MS);
        let left = t;
        let mut next = t + TRIAL_EVERY;
        while s.place() == Place::Cpu && t < left + 60_000 * MS {
            // Its readback is still a frame's wait for the card, but not of the work that left it
            s.frame(50 * MS, t);
            if t >= next {
                s.trial(2 * MS, t);
                next += TRIAL_EVERY;
            }
            t += FRAME;
        }
        assert_eq!(s.place(), Place::Card);
        assert!(t - left <= CLEAR_FOR + 3 * TRIAL_EVERY, "{} ms", (t - left) / MS);
    }

    #[test]
    fn a_stream_that_slows_down_is_held_to_its_new_rate_and_a_card_near_the_line_isnt_brought_back() {
        let mut s = Sim::at(AT_60);
        let mut t = s.frames(LATER, 4, 20 * MS);
        assert_eq!(s.place(), Place::Cpu);
        s.w.interval = INTERVAL;
        let left = t;
        while s.place() == Place::Cpu && t < left + 60_000 * MS {
            s.w.note_cpu(6 * MS, t);
            s.trial(20 * MS, t);
            t += TRIAL_EVERY;
        }
        assert_eq!(s.place(), Place::Card, "20 ms keeps time at 30 a second");
        assert!(t - left <= CLEAR_FOR + 2 * TRIAL_EVERY);
        // A stream that stayed at 30 a second and left a card waiting 30 ms isn't brought back by one waiting 20, near its line
        let mut s = Sim::new();
        let t = s.frames(LATER, 4, 30 * MS);
        s.off_card(t, 60_000 * MS, 6 * MS, 20 * MS);
        assert_eq!(s.place(), Place::Cpu);
    }

    #[test]
    fn what_the_cpu_cost_is_forgotten_when_the_work_changes() {
        let b = Arc::new(Balance::free());
        let lane = b.lane();
        (0..4).for_each(|_| lane.note_wait(60 * MS));
        (0..CPU_SEEN).for_each(|_| lane.note_cpu(55 * MS));
        lane.with(|w| w.place = Place::Card);
        (0..100).for_each(|_| lane.note_wait(40 * MS));
        assert_eq!(lane.place(), Place::Card, "55 ms on the CPU is no better than 40 ms on the card");
        lane.work_changed();
        assert_eq!(lane.cpu_cost(), None);
        (0..4).for_each(|_| lane.note_wait(40 * MS));
        assert_eq!(lane.place(), Place::Cpu, "other steps or another size, so the CPU is tried again");
    }

    #[test]
    fn a_pause_starts_the_trials_stretches_over() {
        let mut s = Sim::new();
        let t = s.frames(LATER, 4, 40 * MS);
        (0..CPU_SEEN as u64).for_each(|i| s.w.note_cpu(30 * MS, t + i));
        s.trial(35 * MS, t + TRIAL_EVERY);
        s.trial(35 * MS, t + 2 * TRIAL_EVERY);
        // Frames stop for less than the gap a missing trial is noticed after, and the stream says when they come again
        let resumed = t + 2 * TRIAL_EVERY + 1_200 * MS;
        s.w.resumed();
        s.trial(35 * MS, resumed);
        assert_eq!(s.place(), Place::Cpu, "the time paused says nothing of the card");
        let mut t = resumed;
        while s.place() == Place::Cpu {
            t += TRIAL_EVERY;
            s.trial(35 * MS, t);
        }
        assert_eq!(t - resumed, NO_WORSE_FOR);
        // A longer gap is noticed without being told
        let mut s = Sim::new();
        let t = s.frames(LATER, 4, 40 * MS);
        (0..CPU_SEEN as u64).for_each(|i| s.w.note_cpu(30 * MS, t + i));
        s.trial(35 * MS, t + TRIAL_EVERY);
        s.trial(35 * MS, t + 10_000 * MS);
        assert_eq!(s.place(), Place::Cpu);
    }

    #[test]
    fn a_pause_on_the_card_isnt_a_stay_that_worked() {
        let mut s = Sim::new();
        let mut t = s.frames(LATER, 4, 40 * MS);
        while s.place() == Place::Cpu {
            s.trial(2 * MS, t);
            t += TRIAL_EVERY;
        }
        let back = s.w.clear_for;
        // Frames stop for a minute (a lost link) without anyone saying so
        t = s.frames(t, 10, 3 * MS) + 60_000 * MS;
        s.frames(t, 4, 40 * MS);
        assert_eq!(s.w.clear_for, (back * 2).min(CLEAR_FOR_MOST), "a minute without frames isn't a minute on the card");
    }

    #[test]
    fn the_cpu_is_only_measured_off_the_card_and_a_move_off_measures_it_again() {
        let mut s = Sim::new();
        (0..20).for_each(|i| s.w.note_cpu(MS, i));
        assert_eq!(s.w.cpu_cost(20), None, "on the card the CPU does none of this work");
        let t = s.frames(0, 4, 40 * MS);
        (0..CPU_SEEN as u64 - 1).for_each(|i| s.w.note_cpu(9 * MS, t + i));
        assert_eq!(s.w.cpu_cost(t + 10), None, "too few to go by");
        s.w.note_cpu(9 * MS, t + 10);
        assert_eq!(s.w.cpu_cost(t + 10), Some(9 * MS));
        assert_eq!(s.w.cpu_cost(t + 10 + CPU_KEPT), None, "too old");
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
        (0..CPU_SEEN).for_each(|_| lane.note_cpu(5 * MS));
        b.keep_on_card(true);
        assert_eq!(lane.cpu_cost(), None, "held on the card, what it cost before is stale");
        lane.note_cpu(5 * MS);
        assert_eq!(lane.with(|w| w.cpu_seen), 0, "and nothing is measured");
        b.keep_on_card(false);
        let other = lane.clone();
        drop(lane);
        assert_eq!(b.lanes.lock().unwrap_or_else(|e| e.into_inner()).watches.len(), 1, "a clone is the same stream");
        drop(other);
        assert!(b.lanes.lock().unwrap_or_else(|e| e.into_inner()).watches.is_empty());
    }
}
