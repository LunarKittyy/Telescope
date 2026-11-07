//! A button or arrow held down on a card: one step at once, then steps while it stays down, bigger after a while. A step is only
//! sent when whatever it moves is ready for it, so a slow one (a camera) is never left with a queue of steps to run long after the
//! hand came off. Counted in timer ticks, so nothing here reads a clock. The cards that step along a list (Webcam, Photos) share it.

/// How often a held button is looked at, in milliseconds.
pub const TICK_MS: u64 = 50;
/// Ticks before a held button starts repeating: about 0.45 s, so a plain click is one step.
const FIRST_TICKS: u32 = 9;
/// Ticks between two repeats at the fastest, about 0.1 s.
pub const REPEAT_TICKS: u32 = 2;
/// Ticks of holding before the steps get bigger: about 1.4 s.
const BIG_AFTER_TICKS: u32 = 28;
/// A press that never saw its release (the window lost the pointer) lets go by itself after about 30 s.
const LONGEST_TICKS: u32 = 600;

/// What a tick says to do.
#[derive(Debug, PartialEq, Eq)]
pub enum Tick {
    /// Not yet, or what it moves is still busy.
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

    /// One timer tick. `settled`: what it moves has finished the last step.
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
