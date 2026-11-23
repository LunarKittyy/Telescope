//! How hard the heavy effects (the person models behind Background and Auto framing) work right now. One owner for the whole app: the stream's steps
//! report how long each frame took, and when frames keep coming late the effects step down a level, which makes them analyse less often. They step
//! back up slowly once there has been room for a while, and more slowly each time stepping back up didn't last. Power-saving effects is a floor
//! under it, and people can switch the automatic part off or keep full quality for one run of the app. Above full quality Auto framing may follow
//! faces instead of whole bodies (allow_heads), and the vision service says when it does (note_heads) so the Adjusting list can show it.

use crate::time::monotonic_ns;
use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};
use std::sync::{Arc, LazyLock, Mutex};

const MS: u64 = 1_000_000;
const SECOND: u64 = 1_000_000_000;

/// Full quality: every effect works as it was made to.
pub const FULL: u8 = 0;
/// The lightest the effects get.
pub const MOST: u8 = 3;

/// How long a window of frames is judged over.
const WINDOW: u64 = SECOND;
/// A window with fewer frames than this says nothing, so a stream that is starting or hardly running moves nothing.
const MIN_FRAMES: u32 = 10;
/// A frame is late when its steps took more than this share of the time a frame has (9 tenths).
const LATE_TENTHS: u64 = 9;
/// A window is behind when more than this share of its frames were late or dropped.
const BEHIND_SHARE: f32 = 0.10;
/// A window is clear when fewer than this share were, and the steps took under half of a frame's time.
const CLEAR_SHARE: f32 = 0.02;
/// How many windows in a row have to be behind before the effects step down.
const BEHIND_WINDOWS: u8 = 2;
/// The least time between two steps down.
const DOWN_GAP: u64 = 3 * SECOND;
/// How long every window has to be clear before the effects step back up, at first and at most.
const CLEAR_FOR: u64 = 10 * SECOND;
const CLEAR_FOR_MOST: u64 = 120 * SECOND;
/// A stay after stepping back up that lasted this long counts as having worked.
const STAYED: u64 = 45 * SECOND;
/// How many moves are remembered.
const KEPT_MOVES: usize = 8;
/// Faces count as followed this long after the last frame they were followed in.
const HEADS_SHOWN: u64 = 2 * SECOND;

/// Why the effects' level changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Why {
    /// Too many frames were late or dropped.
    Behind,
    /// Frames had room for long enough.
    Room,
    /// A setting moved it (power-saving effects, the automatic switch, keeping full quality).
    Override,
}

/// One change of the level, for the stats.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EffortMove {
    /// The monotonic clock when it happened, in ms (the same clock as `Snapshot::at_ms`).
    pub at_ms: u64,
    pub from: u8,
    pub to: u8,
    pub why: Why,
}

/// What the stats show of the effects' level.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct EffortSnapshot {
    /// What the effects run at now.
    pub level: u8,
    /// What the automatic part alone says, whether or not a setting overrides it.
    pub auto_level: u8,
    /// The least level power-saving effects asks for.
    pub floor: u8,
    /// Whether the automatic part is on.
    pub auto: bool,
    /// Full quality is kept for this run of the app.
    pub held: bool,
    /// How many of the newest window's frames were late or dropped, 0 to 1.
    pub late_share: f32,
    /// The latest changes of `level`, oldest first.
    pub moves: Vec<EffortMove>,
    /// Auto framing follows faces instead of whole bodies right now.
    #[serde(default)]
    pub heads: bool,
}

/// What one window of frames showed.
#[derive(Clone, Copy, Debug, Default)]
struct Window {
    /// Frames whose steps ran, how many of them were late, and frames dropped because the steps hadn't taken the one before.
    ran: u32,
    late: u32,
    shed: u32,
    /// The steps' time over the ran frames, and the time a frame has.
    step_ns: u64,
    interval_ns: u64,
    /// How long the window lasted.
    span_ns: u64,
}

/// What the automatic part decided.
struct Step {
    why: Why,
    /// For the log line: the late share when stepping down, how long frames have had room when stepping up.
    late_share: f32,
    clear_for: u64,
}

/// The decision itself, with time passed in.
struct Watch {
    level: u8,
    floor: u8,
    /// How many windows in a row were behind.
    behind: u8,
    /// Since when every window has been clear.
    clear_since: Option<u64>,
    clear_for: u64,
    down_at: Option<u64>,
    /// When the last step up was, until a step down follows it.
    up_at: Option<u64>,
    late_share: f32,
}

impl Watch {
    fn new() -> Watch {
        Watch { level: FULL, floor: FULL, behind: 0, clear_since: None, clear_for: CLEAR_FOR, down_at: None, up_at: None, late_share: 0.0 }
    }

    /// A new stretch of evidence is needed (a setting just changed what the frames cost).
    fn restart(&mut self) {
        self.behind = 0;
        self.clear_since = None;
    }

    fn judge(&mut self, now: u64, w: Window) -> Option<Step> {
        let frames = w.ran + w.shed;
        if frames < MIN_FRAMES {
            // A long gap with hardly any frames means the stream stopped, so what came before says nothing about what comes next
            if w.span_ns > 3 * WINDOW {
                self.restart();
            }
            return None;
        }
        let share = (w.late + w.shed) as f32 / frames as f32;
        self.late_share = share;
        let mean_ns = w.step_ns / w.ran.max(1) as u64;
        if share > BEHIND_SHARE {
            self.clear_since = None;
            self.behind = self.behind.saturating_add(1);
            if self.behind < BEHIND_WINDOWS || self.down_at.is_some_and(|t| now.saturating_sub(t) < DOWN_GAP) {
                return None;
            }
            // From the level in force, so a floor isn't stepped down to as if it were room
            let from = self.level.max(self.floor);
            if from >= MOST {
                return None;
            }
            // Coming back didn't last, so the next time has to wait longer, and a long stay starts over
            self.clear_for = match self.up_at.take() {
                Some(at) if now.saturating_sub(at) < STAYED => (self.clear_for * 2).min(CLEAR_FOR_MOST),
                Some(_) => CLEAR_FOR,
                None => self.clear_for,
            };
            self.level = from + 1;
            self.down_at = Some(now);
            self.behind = 0;
            return Some(Step { why: Why::Behind, late_share: share, clear_for: self.clear_for });
        }
        self.behind = 0;
        if share >= CLEAR_SHARE || mean_ns * 2 >= w.interval_ns {
            self.clear_since = None;
            return None;
        }
        // Nothing to give back at or under the floor
        if self.level <= self.floor {
            self.clear_since = None;
            return None;
        }
        let since = *self.clear_since.get_or_insert(now.saturating_sub(w.span_ns));
        if now.saturating_sub(since) < self.clear_for {
            return None;
        }
        self.level -= 1;
        self.up_at = Some(now);
        self.clear_since = None;
        Some(Step { why: Why::Room, late_share: share, clear_for: self.clear_for })
    }
}

/// Everything behind the lock: the decision, the switches and what happened.
struct State {
    watch: Watch,
    auto: bool,
    held: bool,
    /// When the window now being counted began, 0 before the first.
    window_start: u64,
    moves: VecDeque<EffortMove>,
    /// The level the readers were last told.
    shown: u8,
}

impl State {
    fn level(&self) -> u8 {
        let auto = if self.auto && !self.held { self.watch.level } else { FULL };
        auto.max(self.watch.floor)
    }
}

/// See the top of this file.
pub struct Effort {
    state: Mutex<State>,
    /// The level in force, for what asks every frame.
    level: AtomicU8,
    /// This window's counts, which the steps thread adds to without a lock.
    ran: AtomicU32,
    late: AtomicU32,
    shed: AtomicU32,
    step_ns: AtomicU64,
    interval_ns: AtomicU64,
    /// When this window is over.
    next_window: AtomicU64,
    /// Whether people may be followed by their faces while the effects are lighter, and when they last were (0: never).
    heads_ok: AtomicBool,
    heads_at: AtomicU64,
}

static SHARED: LazyLock<Arc<Effort>> = LazyLock::new(|| Arc::new(Effort::free()));

impl Effort {
    /// The app's one.
    pub fn shared() -> Arc<Effort> {
        SHARED.clone()
    }

    /// The same without taking a reference count, for what runs every frame.
    pub fn global() -> &'static Effort {
        &SHARED
    }

    /// One of its own, for tests.
    pub fn free() -> Effort {
        Effort {
            state: Mutex::new(State { watch: Watch::new(), auto: true, held: false, window_start: 0, moves: VecDeque::new(), shown: FULL }),
            level: AtomicU8::new(FULL),
            ran: AtomicU32::new(0),
            late: AtomicU32::new(0),
            shed: AtomicU32::new(0),
            step_ns: AtomicU64::new(0),
            interval_ns: AtomicU64::new(0),
            next_window: AtomicU64::new(0),
            heads_ok: AtomicBool::new(true),
            heads_at: AtomicU64::new(0),
        }
    }

    /// What the effects run at: 0 is full, up to MOST for the lightest.
    pub fn level(&self) -> u8 {
        self.level.load(Ordering::Relaxed)
    }

    /// The steps ran on a frame and took step_ns, when a frame has interval_ns at the stream's rate.
    pub fn note_frame(&self, step_ns: u64, interval_ns: u64) {
        self.ran.fetch_add(1, Ordering::Relaxed);
        if step_ns.saturating_mul(10) > interval_ns.saturating_mul(LATE_TENTHS) {
            self.late.fetch_add(1, Ordering::Relaxed);
        }
        self.step_ns.fetch_add(step_ns, Ordering::Relaxed);
        self.interval_ns.store(interval_ns, Ordering::Relaxed);
        self.tick(monotonic_ns());
    }

    /// n frames were dropped because the steps hadn't taken the one before.
    pub fn note_shed(&self, n: u64) {
        self.shed.fetch_add(n.min(u32::MAX as u64) as u32, Ordering::Relaxed);
        self.tick(monotonic_ns());
    }

    /// Whether the automatic part moves the level at all (Settings); off, only the floor holds it up.
    pub fn set_auto(&self, on: bool) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if st.auto == on {
            return;
        }
        st.auto = on;
        st.watch.restart();
        if !on {
            st.watch.level = FULL;
        }
        self.settled(&mut st, monotonic_ns(), None);
    }

    /// Keep full quality for this run of the app, whatever the automatic part says; not saved.
    pub fn hold_full(&self, on: bool) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if st.held == on {
            return;
        }
        st.held = on;
        st.watch.restart();
        self.settled(&mut st, monotonic_ns(), None);
    }

    /// The least level the effects run at (power-saving effects asks for 1).
    pub fn set_floor(&self, floor: u8) {
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let floor = floor.min(MOST);
        if st.watch.floor == floor {
            return;
        }
        st.watch.floor = floor;
        st.watch.restart();
        self.settled(&mut st, monotonic_ns(), None);
    }

    /// Whether the vision service may follow faces instead of whole bodies while the effects are lighter (a saved switch, or kept off for one run).
    pub fn allow_heads(&self, on: bool) {
        if self.heads_ok.swap(on, Ordering::Relaxed) != on {
            log::info!("Following faces to save effort {}", if on { "allowed" } else { "not allowed" });
        }
    }

    pub fn heads_allowed(&self) -> bool {
        self.heads_ok.load(Ordering::Relaxed)
    }

    /// The vision service followed a frame's people by their faces.
    pub fn note_heads(&self) {
        self.heads_at.store(monotonic_ns().max(1), Ordering::Relaxed);
    }

    /// Faces were followed within the last HEADS_SHOWN.
    pub fn heads(&self) -> bool {
        let at = self.heads_at.load(Ordering::Relaxed);
        at != 0 && monotonic_ns().saturating_sub(at) < HEADS_SHOWN
    }

    pub fn floor(&self) -> u8 {
        self.state.lock().unwrap_or_else(|p| p.into_inner()).watch.floor
    }

    pub fn snapshot(&self) -> EffortSnapshot {
        let st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        EffortSnapshot {
            level: st.level(),
            auto_level: st.watch.level,
            floor: st.watch.floor,
            auto: st.auto,
            held: st.held,
            late_share: st.watch.late_share,
            moves: st.moves.iter().copied().collect(),
            heads: self.heads(),
        }
    }

    /// Closes the window when its time is up; the rest of the time this is one atomic read.
    fn tick(&self, now: u64) {
        if now < self.next_window.load(Ordering::Relaxed) {
            return;
        }
        let mut st = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if now < self.next_window.load(Ordering::Relaxed) {
            return;
        }
        self.next_window.store(now + WINDOW, Ordering::Relaxed);
        let w = Window {
            ran: self.ran.swap(0, Ordering::Relaxed),
            late: self.late.swap(0, Ordering::Relaxed),
            shed: self.shed.swap(0, Ordering::Relaxed),
            step_ns: self.step_ns.swap(0, Ordering::Relaxed),
            interval_ns: self.interval_ns.load(Ordering::Relaxed),
            span_ns: now.saturating_sub(st.window_start),
        };
        let first = std::mem::replace(&mut st.window_start, now) == 0;
        // The very first window has no start, and while the automatic part is off or held nothing it sees says anything
        if first || !st.auto || st.held {
            return;
        }
        self.judged(&mut st, now, w);
    }

    fn judged(&self, st: &mut State, now: u64, w: Window) {
        let step = st.watch.judge(now, w);
        self.settled(st, now, step);
    }

    /// Tells the readers when the level in force changed, with why: a step of the automatic part, or None for a setting.
    fn settled(&self, st: &mut State, now: u64, step: Option<Step>) {
        let level = st.level();
        let from = std::mem::replace(&mut st.shown, level);
        if level == from {
            return;
        }
        self.level.store(level, Ordering::Relaxed);
        let why = step.as_ref().map_or(Why::Override, |s| s.why);
        if st.moves.len() == KEPT_MOVES {
            st.moves.pop_front();
        }
        st.moves.push_back(EffortMove { at_ms: now / MS, from, to: level, why });
        match step {
            Some(Step { why: Why::Behind, late_share, .. }) => log::info!("Effects stepped down to level {level}: {:.0}% of frames late", late_share * 100.0),
            Some(Step { clear_for, .. }) => log::info!("Effects stepped back up to level {level}: frames have had room for {} s", clear_for / SECOND),
            None => log::info!("Effects set to level {level} by a setting"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: u64 = 33 * MS;

    /// A window of 30 frames, late of them late and shed more dropped, that took step_ms each, lasting a second.
    fn win(late: u32, shed: u32, step_ms: u64) -> Window {
        Window { ran: 30, late, shed, step_ns: 30 * step_ms * MS, interval_ns: FRAME, span_ns: SECOND }
    }

    fn behind() -> Window {
        win(15, 0, 40)
    }

    fn clear() -> Window {
        win(0, 0, 5)
    }

    /// Windows ending at t seconds, one each, from first; returns the levels the watch went to with when.
    fn run(w: &mut Watch, first: u64, windows: &[Window]) -> Vec<(u64, u8)> {
        let mut moved = Vec::new();
        for (i, window) in windows.iter().enumerate() {
            let t = first + i as u64;
            if w.judge(t * SECOND, *window).is_some() {
                moved.push((t, w.level));
            }
        }
        moved
    }

    #[test]
    fn steady_lateness_steps_down_one_level_at_a_time_with_a_gap() {
        let mut w = Watch::new();
        let moved = run(&mut w, 100, &vec![behind(); 30]);
        assert_eq!(moved, [(101, 1), (104, 2), (107, 3)]);
    }

    #[test]
    fn a_single_bad_window_moves_nothing() {
        let mut w = Watch::new();
        let windows: Vec<Window> = (0..100).map(|i| if i % 2 == 0 { behind() } else { clear() }).collect();
        assert!(run(&mut w, 100, &windows).is_empty());
        let windows: Vec<Window> = (0..100).map(|i| if i % 2 == 0 { behind() } else { win(2, 0, 20) }).collect();
        assert!(run(&mut w, 200, &windows).is_empty(), "a window that is neither behind nor clear breaks the run too");
    }

    #[test]
    fn dropped_frames_count_as_late() {
        let mut w = Watch::new();
        let shed = Window { ran: 20, late: 0, shed: 10, step_ns: 20 * 5 * MS, interval_ns: FRAME, span_ns: SECOND };
        assert_eq!(run(&mut w, 100, &[shed, shed]), [(101, 1)]);
    }

    #[test]
    fn windows_with_few_frames_say_nothing() {
        let mut w = Watch::new();
        let few = Window { ran: 9, late: 9, shed: 0, step_ns: 9 * 100 * MS, interval_ns: FRAME, span_ns: SECOND };
        assert!(run(&mut w, 100, &vec![few; 50]).is_empty());
        // One of them between two bad windows neither counts nor breaks the run
        assert_eq!(run(&mut w, 200, &[behind(), few, behind()]), [(202, 1)]);
        // A long gap does break it
        let mut w = Watch::new();
        let gap = Window { span_ns: 10 * SECOND, ..few };
        assert!(run(&mut w, 300, &[behind(), gap, behind()]).is_empty());
    }

    #[test]
    fn room_steps_back_up_after_ten_seconds_of_clear_windows() {
        let mut w = Watch::new();
        run(&mut w, 100, &[behind(), behind()]);
        assert_eq!(w.level, 1);
        assert!(run(&mut w, 200, &vec![clear(); 9]).is_empty(), "not quite long enough");
        assert_eq!(run(&mut w, 209, &[clear()]), [(209, 0)]);
        // A window that is only nearly clear starts the stretch over
        let mut w = Watch::new();
        run(&mut w, 100, &[behind(), behind()]);
        let mut windows = vec![clear(); 9];
        windows.push(win(1, 0, 5));
        windows.extend(vec![clear(); 9]);
        assert!(run(&mut w, 200, &windows).is_empty());
        assert_eq!(run(&mut w, 219, &[clear()]), [(219, 0)]);
        // Steps that take over half a frame aren't room, however few are late
        let mut w = Watch::new();
        run(&mut w, 100, &[behind(), behind()]);
        assert!(run(&mut w, 200, &vec![win(0, 0, 20); 60]).is_empty());
    }

    #[test]
    fn a_step_up_that_was_undone_makes_the_next_wait_longer_and_a_long_stay_clears_that() {
        let mut w = Watch::new();
        let mut t = 100;
        let mut waits = Vec::new();
        for _ in 0..6 {
            // Down, then clear until it steps up again
            while w.level == 0 {
                run(&mut w, t, &[behind()]);
                t += 1;
            }
            waits.push(w.clear_for);
            while w.level > 0 {
                run(&mut w, t, &[clear()]);
                t += 1;
            }
        }
        // The first step down had nothing to undo
        assert_eq!(waits, [10 * SECOND, 20 * SECOND, 40 * SECOND, 80 * SECOND, 120 * SECOND, 120 * SECOND]);
        // Up has worked for a while: back to the start
        t += 60;
        run(&mut w, t, &[behind(), behind()]);
        assert_eq!((w.level, w.clear_for), (1, CLEAR_FOR));
    }

    #[test]
    fn a_floor_is_not_room_to_step_down_to_and_nothing_is_given_back_under_it() {
        let mut w = Watch::new();
        w.floor = 1;
        assert_eq!(run(&mut w, 100, &[behind(); 4]), [(101, 2)]);
        assert_eq!(run(&mut w, 200, &vec![clear(); 100]), [(209, 1)]);
        assert_eq!(w.level, 1);
    }

    #[test]
    fn what_the_effects_run_at_is_the_floor_or_the_automatic_level_whichever_is_higher() {
        let e = Effort::free();
        assert_eq!((e.level(), e.floor()), (FULL, FULL));
        let mut st = e.state.lock().unwrap();
        e.judged(&mut st, 100 * SECOND, behind());
        e.judged(&mut st, 101 * SECOND, behind());
        drop(st);
        assert_eq!(e.level(), 1);
        e.set_floor(2);
        assert_eq!(e.level(), 2);
        e.set_floor(0);
        assert_eq!(e.level(), 1);
        e.set_auto(false);
        assert_eq!((e.level(), e.snapshot().auto_level), (FULL, FULL), "off means the automatic part is 0");
        e.set_floor(1);
        assert_eq!(e.level(), 1, "the floor holds with it off");
        e.set_floor(MOST + 5);
        assert_eq!(e.level(), MOST);
        e.set_floor(0);
        e.set_auto(true);
        assert_eq!(e.level(), FULL);
    }

    #[test]
    fn keeping_full_quality_overrides_the_automatic_part_but_not_the_floor() {
        let e = Effort::free();
        {
            let mut st = e.state.lock().unwrap();
            e.judged(&mut st, 100 * SECOND, behind());
            e.judged(&mut st, 101 * SECOND, behind());
        }
        assert_eq!(e.level(), 1);
        e.hold_full(true);
        assert_eq!(e.level(), FULL);
        assert_eq!(e.snapshot().auto_level, 1, "what it would be is still known");
        e.set_floor(1);
        assert_eq!(e.level(), 1);
        e.set_floor(0);
        e.hold_full(false);
        assert_eq!(e.level(), 1);
    }

    #[test]
    fn the_snapshot_says_what_moved_it_and_why() {
        let e = Effort::free();
        {
            let mut st = e.state.lock().unwrap();
            e.judged(&mut st, 100 * SECOND, behind());
            e.judged(&mut st, 101 * SECOND, behind());
        }
        e.set_floor(2);
        e.set_floor(0);
        {
            let mut st = e.state.lock().unwrap();
            for t in 0..10 {
                e.judged(&mut st, (200 + t) * SECOND, clear());
            }
        }
        let snap = e.snapshot();
        let moves: Vec<_> = snap.moves.iter().map(|m| (m.from, m.to, m.why)).collect();
        assert_eq!(moves, [(0, 1, Why::Behind), (1, 2, Why::Override), (2, 1, Why::Override), (1, 0, Why::Room)]);
        assert_eq!(snap.moves[0].at_ms, 101_000);
        assert_eq!((snap.level, snap.auto_level, snap.floor, snap.auto, snap.held), (0, 0, 0, true, false));
        assert_eq!(snap.late_share, 0.0);
        let json = serde_json::to_string(&snap).unwrap();
        assert_eq!(serde_json::from_str::<EffortSnapshot>(&json).unwrap(), snap);
    }

    #[test]
    fn only_the_newest_moves_are_kept() {
        let e = Effort::free();
        for i in 0..20 {
            e.set_floor(i % 2 + 1);
            e.set_floor(0);
        }
        assert_eq!(e.snapshot().moves.len(), KEPT_MOVES);
    }

    #[test]
    fn frames_are_counted_into_windows_and_late_ones_are_the_ones_over_nine_tenths() {
        let e = Effort::free();
        // The first one opens the first window, which says nothing, and starts the count over
        e.note_frame(MS, FRAME);
        e.note_frame(29 * MS, FRAME);
        e.note_frame(31 * MS, FRAME);
        e.note_shed(3);
        assert_eq!((e.ran.load(Ordering::Relaxed), e.late.load(Ordering::Relaxed), e.shed.load(Ordering::Relaxed)), (2, 1, 3));
        assert_eq!(e.level(), FULL);
    }

    #[test]
    fn following_faces_is_allowed_until_told_otherwise_and_shows_for_a_moment_after() {
        let e = Effort::free();
        assert!(e.heads_allowed() && !e.heads() && !e.snapshot().heads);
        e.allow_heads(false);
        assert!(!e.heads_allowed());
        e.note_heads();
        assert!(e.heads() && e.snapshot().heads);
        // Long after the last frame it isn't in effect any more
        e.heads_at.store(monotonic_ns().saturating_sub(HEADS_SHOWN + SECOND).max(1), Ordering::Relaxed);
        assert!(!e.heads());
    }

    #[test]
    fn an_older_snapshot_without_heads_still_reads() {
        let old: EffortSnapshot = serde_json::from_str(r#"{"level":1,"auto_level":0,"floor":1,"auto":true,"held":false,"late_share":0.0,"moves":[]}"#).unwrap();
        assert!(!old.heads);
    }
}
