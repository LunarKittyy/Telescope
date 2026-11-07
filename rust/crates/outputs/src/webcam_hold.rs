//! Things held on the Webcam card: the arrows of a DSLR's dials (one step at once, then steps while it stays down, bigger after a
//! while) and the focus jog (a step each time the camera is ready, as big as how far the handle is out). Neither sends a step before
//! the camera has finished the last one: a slow camera would otherwise be left with a queue of steps to run long after the hand came
//! off. Counted in timer ticks, so nothing here reads a clock.

use crate::webcam_capture::gphoto_lib::JOG_LEVELS;

/// How often a held button is looked at, in milliseconds.
pub const TICK_MS: u64 = 50;
/// Ticks before a held button starts repeating: about 0.45 s, so a plain click is one step.
const FIRST_TICKS: u32 = 9;
/// Ticks between two repeats at the fastest, about 0.1 s.
const REPEAT_TICKS: u32 = 2;
/// Ticks of holding before the steps get bigger: about 1.4 s.
const BIG_AFTER_TICKS: u32 = 28;
/// A press that never saw its release (the window lost the pointer) lets go by itself after about 30 s.
const LONGEST_TICKS: u32 = 600;

/// What a tick says to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Tick {
    /// Not yet, or the camera is still busy.
    Wait,
    /// Another step, a big one once it has been held a while.
    Step { big: bool },
    /// Stop looking: it has been held longer than anyone does.
    Release,
}

/// One held button: which control and which way (-1 or 1).
#[derive(Debug, PartialEq, Eq)]
pub struct Hold {
    pub name: String,
    pub dir: i32,
    ticks: u32,
    last_step: u32,
}

impl Hold {
    pub fn new(name: &str, dir: i32) -> Hold {
        Hold { name: name.into(), dir: dir.signum(), ticks: 0, last_step: 0 }
    }

    /// One timer tick. `settled`: the camera has finished what it was last asked.
    pub fn tick(&mut self, settled: bool) -> Tick {
        self.ticks += 1;
        if self.ticks > LONGEST_TICKS {
            return Tick::Release;
        }
        if self.ticks < FIRST_TICKS || !settled || self.ticks - self.last_step < REPEAT_TICKS {
            return Tick::Wait;
        }
        self.last_step = self.ticks;
        Tick::Step { big: self.ticks >= BIG_AFTER_TICKS }
    }
}

/// How far out the jog's handle has to be before it asks for anything, as a fraction of its way to the end: a resting hand wobbles.
const JOG_DEAD: f64 = 0.08;
/// A jog held still for about 60 s without a word from the card (the window lost the pointer) lets go by itself.
const JOG_LONGEST_TICKS: u32 = 1200;

/// The jog level (-JOG_LEVELS to JOG_LEVELS, negative toward Near, 0 at rest) for a handle `offset` of the way out to either end (-1 to 1):
/// nothing inside the dead zone, then 1 just outside it rising evenly to the last level at the end.
pub fn jog_level(offset: f64) -> i64 {
    let out = offset.abs();
    if !out.is_finite() || out < JOG_DEAD {
        return 0;
    }
    let along = ((out - JOG_DEAD) / (1.0 - JOG_DEAD)).clamp(0.0, 1.0);
    let level = 1 + (along * (JOG_LEVELS - 1) as f64).round() as i64;
    if offset < 0.0 { -level } else { level }
}

/// What a tick of a held jog says to do.
#[derive(Debug, PartialEq, Eq)]
pub enum JogTick {
    /// The camera is still busy.
    Wait,
    /// Another step, at this level.
    Step { level: i64 },
    /// Stop looking.
    Release,
}

/// The jog held out at some level, on the control called `name`. Its level follows the hand until it comes back to the middle.
#[derive(Debug, PartialEq, Eq)]
pub struct JogHold {
    pub name: String,
    level: i64,
    /// Ticks since the level last changed, and since the last step.
    held: u32,
    since_step: u32,
}

impl JogHold {
    /// Ready to step on the first tick: the handle is already out.
    pub fn new(name: &str, level: i64) -> JogHold {
        JogHold { name: name.into(), level, held: 0, since_step: REPEAT_TICKS }
    }

    pub fn set_level(&mut self, level: i64) {
        if level != self.level {
            self.level = level;
            self.held = 0;
        }
    }

    /// One timer tick. `settled`: the camera has finished what it was last asked. Only ever one step at a time, as big as the handle's
    /// level now, so how fast the focus moves is the step's size and how quickly the camera answers.
    pub fn tick(&mut self, settled: bool) -> JogTick {
        self.held += 1;
        self.since_step += 1;
        if self.held > JOG_LONGEST_TICKS {
            return JogTick::Release;
        }
        if !settled || self.level == 0 || self.since_step < REPEAT_TICKS {
            return JogTick::Wait;
        }
        self.since_step = 0;
        JogTick::Step { level: self.level }
    }
}

/// Where a step of `by` entries from entry `at` in a list of `len` lands, or None when it's already at that end.
pub fn stepped(at: usize, len: usize, dir: i32, by: usize) -> Option<usize> {
    let to = if dir < 0 { at.saturating_sub(by) } else { (at + by).min(len.saturating_sub(1)) };
    (to != at && len > 0).then_some(to)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(hold: &mut Hold, ticks: u32, settled: impl Fn(u32) -> bool) -> Vec<(u32, Tick)> {
        (1..=ticks).filter_map(|i| match hold.tick(settled(i)) {
            Tick::Wait => None,
            t => Some((i, t)),
        }).collect()
    }

    #[test]
    fn nothing_repeats_before_the_first_pause_so_a_click_is_one_step() {
        let mut hold = Hold::new("dslr_focus", -1);
        assert!(run(&mut hold, FIRST_TICKS - 1, |_| true).is_empty());
    }

    #[test]
    fn a_held_button_repeats_then_takes_bigger_steps() {
        let mut hold = Hold::new("dslr_focus", 1);
        let steps = run(&mut hold, 40, |_| true);
        assert_eq!(steps[0], (FIRST_TICKS, Tick::Step { big: false }));
        // Every REPEAT_TICKS after that, as fast as it is allowed
        assert!(steps.windows(2).all(|w| w[1].0 - w[0].0 == REPEAT_TICKS), "{steps:?}");
        let (small, big): (Vec<_>, Vec<_>) = steps.iter().partition(|(_, t)| *t == Tick::Step { big: false });
        assert!(small.iter().all(|(i, _)| *i < BIG_AFTER_TICKS) && big.iter().all(|(i, _)| *i >= BIG_AFTER_TICKS));
        assert!(!small.is_empty() && !big.is_empty());
    }

    // The camera takes a second over each step: the button, held for ten seconds, still sends one at a time
    #[test]
    fn a_step_waits_until_the_camera_has_finished_the_last_one() {
        let mut hold = Hold::new("dslr_focus", -1);
        // Busy for 20 ticks after each step it takes
        let mut busy_until = 0;
        let mut sent = Vec::new();
        for i in 1..=200u32 {
            if let Tick::Step { .. } = hold.tick(i >= busy_until) {
                assert!(i >= busy_until, "sent a step at {i} while the camera was busy until {busy_until}");
                sent.push(i);
                busy_until = i + 20;
            }
        }
        assert_eq!(sent.len(), 10, "{sent:?}");
        assert!(sent.windows(2).all(|w| w[1] - w[0] >= 20));
    }

    #[test]
    fn a_camera_that_stays_busy_gets_no_backlog_and_a_step_when_it_frees_up() {
        let mut hold = Hold::new("dslr_aperture", 1);
        assert!(run(&mut hold, 100, |_| false).is_empty());
        assert_eq!(hold.tick(true), Tick::Step { big: true });
    }

    #[test]
    fn a_press_that_never_saw_its_release_lets_go() {
        let mut hold = Hold::new("dslr_focus", 1);
        let all = run(&mut hold, LONGEST_TICKS + 1, |_| false);
        assert_eq!(all, [(LONGEST_TICKS + 1, Tick::Release)]);
    }

    #[test]
    fn the_handle_asks_for_nothing_near_the_middle_then_more_the_further_out_it_is() {
        assert_eq!((jog_level(0.0), jog_level(0.05), jog_level(-0.07), jog_level(f64::NAN)), (0, 0, 0, 0));
        assert_eq!((jog_level(0.09), jog_level(-0.09)), (1, -1), "just outside the dead zone is the smallest step, either way");
        assert_eq!((jog_level(1.0), jog_level(-1.0), jog_level(7.0), jog_level(-7.0)), (JOG_LEVELS, -JOG_LEVELS, JOG_LEVELS, -JOG_LEVELS));
        let levels: Vec<i64> = (0..=100).map(|i| jog_level(i as f64 / 100.0)).collect();
        assert!(levels.windows(2).all(|w| w[0] <= w[1] && w[1] - w[0] <= 1), "never a lurch of two levels: {levels:?}");
        assert_eq!(levels.iter().copied().collect::<std::collections::BTreeSet<_>>().len() as i64, JOG_LEVELS + 1, "every level can be reached");
        assert!(levels.windows(2).filter(|w| w[0] != w[1]).count() as i64 == JOG_LEVELS);
    }

    #[test]
    fn a_jog_steps_at_once_and_then_each_time_the_camera_is_ready_at_the_size_the_handle_is_out() {
        let mut jog = JogHold::new("dslr_focus", 3);
        assert_eq!(jog.tick(true), JogTick::Step { level: 3 }, "the first tick steps: the handle is already out");
        assert_eq!(jog.tick(true), JogTick::Wait, "a step is never closer than REPEAT_TICKS after the last");
        assert_eq!(jog.tick(true), JogTick::Step { level: 3 });
        // Dragged further, the next step is bigger; nothing is waiting to be sent
        jog.set_level(9);
        assert_eq!(jog.tick(true), JogTick::Wait);
        assert_eq!(jog.tick(true), JogTick::Step { level: 9 });
        jog.set_level(-4);
        jog.tick(true);
        assert_eq!(jog.tick(true), JogTick::Step { level: -4 }, "and the other way");
    }

    #[test]
    fn a_jog_that_sprang_back_to_the_middle_sends_nothing_more() {
        let mut jog = JogHold::new("dslr_focus", 12);
        assert_eq!(jog.tick(true), JogTick::Step { level: 12 });
        jog.set_level(0);
        assert!((0..200).all(|_| jog.tick(true) == JogTick::Wait));
    }

    #[test]
    fn a_jog_never_queues_steps_for_a_camera_that_is_busy() {
        let mut jog = JogHold::new("dslr_focus", 12);
        // Busy for 20 ticks after each step, as a slow camera is: held out all the way for 200 ticks it still gets one step at a time
        let mut busy_until = 0;
        let mut sent = Vec::new();
        for i in 1..=200u32 {
            if let JogTick::Step { .. } = jog.tick(i >= busy_until) {
                assert!(i >= busy_until, "sent a step at {i} while the camera was busy until {busy_until}");
                sent.push(i);
                busy_until = i + 20;
            }
        }
        assert_eq!(sent.len(), 10, "{sent:?}");
        // And one that stays busy has nothing waiting for it when it frees up beyond the one step
        let mut jog = JogHold::new("dslr_focus", 5);
        assert!((0..100).all(|_| jog.tick(false) == JogTick::Wait));
        assert_eq!(jog.tick(true), JogTick::Step { level: 5 });
        assert_eq!(jog.tick(true), JogTick::Wait);
    }

    #[test]
    fn a_jog_whose_release_never_came_lets_go_but_one_that_keeps_moving_does_not() {
        let mut jog = JogHold::new("dslr_focus", 2);
        assert!((0..JOG_LONGEST_TICKS).all(|_| jog.tick(false) == JogTick::Wait));
        assert_eq!(jog.tick(false), JogTick::Release);
        let mut moving = JogHold::new("dslr_focus", 2);
        for i in 0..(JOG_LONGEST_TICKS * 3) {
            moving.set_level(if i % 2 == 0 { 2 } else { 3 });
            assert_ne!(moving.tick(true), JogTick::Release);
        }
    }

    #[test]
    fn a_direction_is_only_ever_one_either_way() {
        assert_eq!((Hold::new("x", -7).dir, Hold::new("x", 3).dir), (-1, 1));
    }

    #[test]
    fn a_step_along_a_list_stops_at_its_ends() {
        assert_eq!(stepped(5, 17, 1, 1), Some(6));
        assert_eq!(stepped(5, 17, -1, 1), Some(4));
        assert_eq!(stepped(15, 31, 1, 3), Some(18));
        assert_eq!(stepped(29, 31, 1, 3), Some(30), "a jump that would pass the end goes to it");
        assert_eq!((stepped(0, 17, -1, 1), stepped(16, 17, 1, 1), stepped(0, 0, 1, 1), stepped(1, 31, -1, 3)), (None, None, None, Some(0)));
    }
}
