//! The focus jog on the Webcam card: a step each time the camera is ready, as big as how far the handle is out. It never sends a step
//! before the camera has finished the last one: a slow camera would otherwise be left with a queue of steps to run long after the
//! hand came off. Counted in timer ticks, so nothing here reads a clock. The arrows of the dials use `telescope_core::hold`.

use crate::webcam_capture::gphoto_lib::JOG_LEVELS;
use telescope_core::hold::REPEAT_TICKS;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
