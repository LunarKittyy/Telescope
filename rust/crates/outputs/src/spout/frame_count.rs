//! Telling new pictures from ones already read, by the frame counter a Spout sender keeps. Plain logic with no Windows in it, so it's
//! checked on every system.

/// Which count the last picture read came with.
#[derive(Default)]
pub struct FrameTracker {
    seen: Option<i32>,
}

impl FrameTracker {
    /// Whether a picture may be waiting, given the counter's reading now. A sender that doesn't count (no counter, or a count of 0)
    /// has made one every time.
    pub fn may_be_new(&self, count: Option<i32>) -> bool {
        match count {
            Some(count) if count != 0 => self.seen != Some(count),
            _ => true,
        }
    }

    /// Notes that the picture that came with `count` was read. Only a picture that was really read counts, so one the sender made
    /// while its texture couldn't be locked is still waited for.
    pub fn read(&mut self, count: Option<i32>) {
        if let Some(count) = count.filter(|c| *c != 0) {
            self.seen = Some(count);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_not_read_yet_is_new_and_the_same_count_after_reading_is_not() {
        let mut tracker = FrameTracker::default();
        assert!(tracker.may_be_new(Some(5)));
        tracker.read(Some(5));
        assert!(!tracker.may_be_new(Some(5)));
        assert!(tracker.may_be_new(Some(6)));
    }

    #[test]
    fn a_sender_that_doesnt_count_always_has_a_new_picture() {
        let mut tracker = FrameTracker::default();
        for count in [None, Some(0)] {
            tracker.read(count);
            assert!(tracker.may_be_new(count));
        }
    }

    #[test]
    fn a_picture_that_wasnt_read_is_still_waited_for() {
        let mut tracker = FrameTracker::default();
        tracker.read(Some(1));
        // The sender makes picture 2, the texture lock times out, the sender then goes idle: nothing was read, so the count stays new
        for _ in 0..3 {
            assert!(tracker.may_be_new(Some(2)));
        }
        tracker.read(Some(2));
        assert!(!tracker.may_be_new(Some(2)));
    }
}
