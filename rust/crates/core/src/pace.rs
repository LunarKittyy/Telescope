//! Even timing for a source that sends in bursts: frames leave about as far apart as they were made, a little later than they came.

use std::collections::VecDeque;

/// Frames looked back over, about four seconds.
const WINDOW: usize = 120;
/// Frames seen before anything is held: a stream starts with no delay.
const SETTLE: usize = 24;
/// The most a frame is held, in frame lengths.
const MAX_HOLD_FRAMES: u64 = 2;
/// A frame this late counts for nothing, as a share of a frame length: a sender this even is left alone.
const EVEN_SHARE: u64 = 4;
/// The share of frames, in percent, that should be here by the time they leave.
const COVERED_PERCENT: usize = 90;
/// A pause this long, or the sender's clock jumping, starts over.
const RESTART_NS: u64 = 1_000_000_000;
/// How fast the hold changes per frame: up quickly, down slowly, and never in a step anyone sees.
const RISE_NS: u64 = 2_000_000;
const FALL_NS: u64 = 500_000;

/// Works out when each frame of one stream should leave, from when it was made on the sender's clock and when it arrived here.
#[derive(Default)]
pub struct EvenTiming {
    /// (made, arrived) of the last WINDOW frames.
    seen: VecDeque<(u64, u64)>,
    hold_ns: u64,
}

impl EvenTiming {
    pub fn new() -> EvenTiming {
        EvenTiming::default()
    }

    /// How far behind its best time a frame is currently released.
    pub fn hold_ns(&self) -> u64 {
        self.hold_ns
    }

    /// When the frame should leave: never before it arrived, and a frame that's late for that goes at once.
    pub fn due(&mut self, made_ns: u64, arrived_ns: u64) -> u64 {
        if let Some(&(made, arrived)) = self.seen.back() {
            if made_ns <= made || made_ns - made > RESTART_NS || arrived_ns.saturating_sub(arrived) > RESTART_NS {
                self.seen.clear();
                self.hold_ns = 0;
            }
        }
        self.seen.push_back((made_ns, arrived_ns));
        if self.seen.len() > WINDOW {
            self.seen.pop_front();
        }
        if self.seen.len() < SETTLE {
            return arrived_ns;
        }
        // The way here at its quickest lately, in the difference between the two clocks
        let best = self.seen.iter().map(|&(made, arrived)| arrived as i64 - made as i64).min().unwrap_or(0);
        let mut late: Vec<u64> = self.seen.iter().map(|&(made, arrived)| (arrived as i64 - made as i64 - best) as u64).collect();
        late.sort_unstable();
        let first = self.seen.front().map_or(made_ns, |s| s.0);
        let frame_ns = (made_ns - first) / (self.seen.len() as u64 - 1);
        let most = late[late.len() * COVERED_PERCENT / 100];
        let target = if most < frame_ns / EVEN_SHARE { 0 } else { most.min(frame_ns * MAX_HOLD_FRAMES) };
        self.hold_ns = if target > self.hold_ns { target.min(self.hold_ns + RISE_NS) } else { target.max(self.hold_ns.saturating_sub(FALL_NS)) };
        let due = (made_ns as i64 + best + self.hold_ns as i64) as u64;
        due.clamp(arrived_ns, arrived_ns + frame_ns * MAX_HOLD_FRAMES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: u64 = 40_000_000;
    const START: u64 = 5_000_000_000;

    /// Runs frames through and gives the gaps between when they left, past the time the hold takes to settle.
    fn gaps(count: u64, late: impl Fn(u64) -> u64) -> (Vec<u64>, u64) {
        let mut pace = EvenTiming::new();
        let due: Vec<u64> = (0..count).map(|i| pace.due(i * FRAME, START + i * FRAME + late(i))).collect();
        let settled = &due[due.len() / 2..];
        (settled.windows(2).map(|w| w[1] - w[0]).collect(), pace.hold_ns())
    }

    #[test]
    fn a_sender_that_arrives_evenly_is_never_held() {
        let mut pace = EvenTiming::new();
        for i in 0..400 {
            let arrived = START + i * FRAME + (i % 3) * 2_000_000;
            assert_eq!(pace.due(i * FRAME, arrived), arrived);
        }
        assert_eq!(pace.hold_ns(), 0);
    }

    #[test]
    fn a_stream_starts_with_no_delay_even_in_bursts() {
        let mut pace = EvenTiming::new();
        for i in 0..SETTLE as u64 - 1 {
            let arrived = START + i * FRAME + (i % 2) * 30_000_000;
            assert_eq!(pace.due(i * FRAME, arrived), arrived);
        }
    }

    #[test]
    fn frames_arriving_in_pairs_leave_evenly() {
        let (gaps, hold) = gaps(600, |i| if i % 2 == 0 { 30_000_000 } else { 0 });
        assert!(gaps.iter().all(|g| g.abs_diff(FRAME) <= 3_000_000), "{gaps:?}");
        assert!((30_000_000..=FRAME).contains(&hold), "{hold}");
    }

    #[test]
    fn the_hold_never_passes_two_frames() {
        let mut pace = EvenTiming::new();
        for i in 0..600 {
            let arrived = START + i * FRAME + (i % 5) * 60_000_000;
            let due = pace.due(i * FRAME, arrived);
            assert!(due <= arrived + 2 * FRAME && due >= arrived);
        }
        assert!(pace.hold_ns() <= 2 * FRAME);
    }

    #[test]
    fn a_late_frame_goes_at_once_and_a_few_of_them_add_no_delay() {
        let late_one = |i: u64| if i % 40 == 39 { 200_000_000 } else { 0 };
        let mut pace = EvenTiming::new();
        for i in 0..400 {
            let arrived = START + i * FRAME + late_one(i);
            let due = pace.due(i * FRAME, arrived);
            if late_one(i) > 0 {
                assert_eq!(due, arrived);
            }
        }
        assert_eq!(pace.hold_ns(), 0);
    }

    #[test]
    fn the_hold_goes_away_again_once_the_sender_evens_out() {
        let mut pace = EvenTiming::new();
        for i in 0..300 {
            pace.due(i * FRAME, START + i * FRAME + (i % 2) * 30_000_000);
        }
        assert!(pace.hold_ns() > 0);
        for i in 300..800 {
            pace.due(i * FRAME, START + i * FRAME + 30_000_000);
        }
        assert_eq!(pace.hold_ns(), 0);
    }

    #[test]
    fn a_pause_or_a_new_clock_starts_over() {
        let mut pace = EvenTiming::new();
        for i in 0..300 {
            pace.due(i * FRAME, START + i * FRAME + (i % 2) * 30_000_000);
        }
        assert!(pace.hold_ns() > 0);
        let arrived = START + 400 * FRAME;
        assert_eq!(pace.due(5, arrived), arrived);
        assert_eq!(pace.hold_ns(), 0);
    }

    #[test]
    fn a_stream_with_no_times_passes_straight_through() {
        let mut pace = EvenTiming::new();
        for i in 0..200 {
            assert_eq!(pace.due(0, START + i * FRAME), START + i * FRAME);
        }
    }

    #[test]
    fn a_drifting_clock_doesnt_build_up_a_hold() {
        let mut pace = EvenTiming::new();
        for i in 0..2000 {
            let arrived = START + i * FRAME + i * 4_000;
            assert_eq!(pace.due(i * FRAME, arrived), arrived);
        }
    }
}
