//! Whether a decoder keeps up with the stream: how long each picture takes to decode (and come off the card), judged over a few seconds.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

/// How long the pictures are watched before a decoder is called too slow.
pub const WINDOW: Duration = Duration::from_secs(3);
/// A decoder whose typical picture takes more than this share of the frame interval can't keep up once anything else needs the machine.
const SHARE: f64 = 0.8;
/// Fewest pictures a verdict needs, so a few slow ones at the start say nothing.
const MIN_PICTURES: usize = 30;

/// How a decoder did over a stretch of pictures, in milliseconds a picture.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Speed {
    pub median_ms: f64,
    pub p95_ms: f64,
    pub max_ms: f64,
    pub pictures: usize,
}

/// The speed of these times (milliseconds), or None for no times.
pub fn speed(times_ms: &[f64]) -> Option<Speed> {
    if times_ms.is_empty() {
        return None;
    }
    let mut sorted = times_ms.to_vec();
    sorted.sort_by(f64::total_cmp);
    let at = |share: f64| sorted[((sorted.len() as f64 * share).ceil() as usize).clamp(1, sorted.len()) - 1];
    Some(Speed { median_ms: at(0.5), p95_ms: at(0.95), max_ms: sorted[sorted.len() - 1], pictures: sorted.len() })
}

/// The speed when the typical picture takes too much of the frame interval to keep up with the stream, else None.
pub fn too_slow(times_ms: &[f64], interval_ms: f64) -> Option<Speed> {
    if times_ms.len() < MIN_PICTURES || interval_ms <= 0.0 {
        return None;
    }
    speed(times_ms).filter(|s| s.median_ms > interval_ms * SHARE)
}

/// A picture this slow is a stall on its own, a whole half second of video late.
const STALL_MS: f64 = 500.0;
/// Pictures this slow count towards a stall when there are two of them within STALL_WINDOW.
const HICCUP_MS: f64 = 200.0;
pub const STALL_WINDOW: Duration = Duration::from_secs(5);

/// The worst of these picture times (milliseconds, all from one STALL_WINDOW) when they add up to a stall: one over 500 ms, or two over 200 ms.
pub fn stall(hiccups_ms: &[f64]) -> Option<f64> {
    let worst = hiccups_ms.iter().copied().fold(0.0, f64::max);
    let heavy = hiccups_ms.iter().filter(|ms| **ms >= HICCUP_MS).count();
    (worst >= STALL_MS || heavy >= 2).then_some(worst)
}

/// How long software decodes after the card was too slow, the first time.
pub const FIRST_WAIT: Duration = Duration::from_secs(5 * 60);
/// The longest wait, after the card keeps being too slow.
pub const LONGEST_WAIT: Duration = Duration::from_secs(40 * 60);
/// A card that's too slow again within this long of coming back waits twice as long next time.
pub const QUICK_REPEAT: Duration = Duration::from_secs(10 * 60);

/// When a card that was too slow gets another go: five minutes, then twice as long each time it falls back soon after returning.
pub struct Cooldown {
    wait: Duration,
    until: Option<Instant>,
    returned_at: Option<Instant>,
}

impl Cooldown {
    pub const fn new() -> Cooldown {
        Cooldown { wait: FIRST_WAIT, until: None, returned_at: None }
    }

    /// The card was too slow now: how long software decodes before it's tried again.
    pub fn fell(&mut self, now: Instant) -> Duration {
        let soon_again = self.returned_at.is_some_and(|at| now.duration_since(at) < QUICK_REPEAT);
        self.wait = if soon_again { (self.wait * 2).min(LONGEST_WAIT) } else { FIRST_WAIT };
        self.until = Some(now + self.wait);
        self.returned_at = None;
        self.wait
    }

    /// Software decodes because the card was too slow, and the wait isn't over.
    pub fn waiting(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| now < until)
    }

    /// Software decodes because the card was too slow, whether or not the wait is over.
    pub fn active(&self) -> bool {
        self.until.is_some()
    }

    /// The wait is over but software was struggling too: a whole wait more.
    pub fn extend(&mut self, now: Instant) {
        self.until = Some(now + self.wait);
    }

    /// The card is back.
    pub fn returned(&mut self, now: Instant) {
        self.until = None;
        self.returned_at = Some(now);
    }
}

/// What a decoder took for its recent pictures.
#[derive(Default)]
pub struct Timings {
    recent: VecDeque<(Instant, f64)>,
    /// Since when the pictures have been timed, so the verdict waits for a whole window.
    since: Option<Instant>,
    /// Pictures that took at least HICCUP_MS, over the last STALL_WINDOW.
    hiccups: VecDeque<(Instant, f64)>,
}

impl Timings {
    /// One picture that took ms.
    pub fn record(&mut self, now: Instant, ms: f64) {
        self.since.get_or_insert(now);
        self.recent.push_back((now, ms));
        while self.recent.front().is_some_and(|(t, _)| now.duration_since(*t) > WINDOW) {
            self.recent.pop_front();
        }
    }

    /// The speed when a whole window of pictures has been timed and they were too slow for the frame interval.
    pub fn slow(&self, now: Instant, interval_ms: f64) -> Option<Speed> {
        if now.duration_since(self.since?) < WINDOW {
            return None;
        }
        too_slow(&self.times(), interval_ms)
    }

    /// The speed over the pictures of the last window.
    pub fn recent(&self) -> Option<Speed> {
        speed(&self.times())
    }

    /// A picture that took ms, counted once however many copies of it record() gets, for the stall check.
    pub fn note_hiccup(&mut self, now: Instant, ms: f64) {
        if ms >= HICCUP_MS {
            self.hiccups.push_back((now, ms));
        }
        while self.hiccups.front().is_some_and(|(t, _)| now.duration_since(*t) > STALL_WINDOW) {
            self.hiccups.pop_front();
        }
    }

    /// The worst picture time (milliseconds) when the decoder stalled lately, else None.
    pub fn stalled(&self) -> Option<f64> {
        stall(&self.hiccups.iter().map(|(_, ms)| *ms).collect::<Vec<_>>())
    }

    fn times(&self) -> Vec<f64> {
        self.recent.iter().map(|(_, ms)| *ms).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speed_reads_the_median_p95_and_max() {
        let times: Vec<f64> = (1..=100).map(f64::from).collect();
        let s = speed(&times).unwrap();
        assert_eq!((s.median_ms, s.p95_ms, s.max_ms, s.pictures), (50.0, 95.0, 100.0, 100));
        assert!(speed(&[]).is_none());
    }

    #[test]
    fn one_picture_half_a_second_late_is_a_stall() {
        assert_eq!(stall(&[520.0]), Some(520.0));
        assert_eq!(stall(&[450.0]), None);
        assert_eq!(stall(&[]), None);
    }

    #[test]
    fn two_pictures_a_fifth_of_a_second_late_are_a_stall() {
        assert_eq!(stall(&[220.0, 300.0]), Some(300.0));
        assert_eq!(stall(&[220.0]), None);
    }

    #[test]
    fn hiccups_older_than_the_stall_window_are_forgotten() {
        let start = Instant::now();
        let mut t = Timings::default();
        t.note_hiccup(start, 250.0);
        t.note_hiccup(start + Duration::from_secs(1), 250.0);
        assert!(t.stalled().is_some());
        t.note_hiccup(start + Duration::from_secs(8), 10.0);
        assert!(t.stalled().is_none());
    }

    #[test]
    fn the_card_waits_five_minutes_then_double_each_quick_repeat_up_to_forty() {
        let t0 = Instant::now();
        let mut c = Cooldown::new();
        let mut now = t0;
        let mut waits = Vec::new();
        for _ in 0..6 {
            let wait = c.fell(now);
            waits.push(wait.as_secs() / 60);
            assert!(c.waiting(now) && c.active());
            now += wait;
            assert!(!c.waiting(now) && c.active());
            c.returned(now);
            assert!(!c.active());
            // Falls back again a minute after returning
            now += Duration::from_secs(60);
        }
        assert_eq!(waits, [5, 10, 20, 40, 40, 40]);
    }

    #[test]
    fn a_card_that_ran_well_a_while_starts_over_at_five_minutes() {
        let t0 = Instant::now();
        let mut c = Cooldown::new();
        c.fell(t0);
        c.fell(t0);
        c.returned(t0 + Duration::from_secs(300));
        assert_eq!(c.fell(t0 + Duration::from_secs(300) + QUICK_REPEAT), FIRST_WAIT);
    }

    #[test]
    fn a_wait_can_be_extended() {
        let t0 = Instant::now();
        let mut c = Cooldown::new();
        c.fell(t0);
        let later = t0 + FIRST_WAIT;
        assert!(!c.waiting(later));
        c.extend(later);
        assert!(c.waiting(later + Duration::from_secs(1)));
    }

    #[test]
    fn a_decoder_well_inside_the_interval_is_fine() {
        assert!(too_slow(&[10.0; 100], 33.3).is_none());
    }

    #[test]
    fn a_decoder_near_the_interval_is_too_slow() {
        let s = too_slow(&[28.0; 100], 33.3).unwrap();
        assert_eq!(s.median_ms, 28.0);
    }

    #[test]
    fn a_few_spikes_dont_make_a_decoder_slow() {
        let mut times = vec![10.0; 90];
        times.extend([80.0; 10]);
        assert!(too_slow(&times, 33.3).is_none());
    }

    #[test]
    fn too_few_pictures_say_nothing() {
        assert!(too_slow(&[100.0; MIN_PICTURES - 1], 33.3).is_none());
        assert!(too_slow(&[100.0; MIN_PICTURES], 33.3).is_some());
    }

    #[test]
    fn a_faster_stream_has_less_time_a_picture() {
        assert!(too_slow(&[20.0; 100], 33.3).is_none());
        assert!(too_slow(&[20.0; 100], 16.7).is_some());
    }

    #[test]
    fn the_verdict_waits_for_a_whole_window() {
        let start = Instant::now();
        let mut t = Timings::default();
        for i in 0..60 {
            t.record(start + Duration::from_millis(i * 20), 40.0);
        }
        assert!(t.slow(start + Duration::from_millis(1200), 33.3).is_none());
        for i in 60..160 {
            t.record(start + Duration::from_millis(i * 20), 40.0);
        }
        assert!(t.slow(start + Duration::from_millis(3200), 33.3).is_some());
    }

    #[test]
    fn old_pictures_leave_the_window() {
        let start = Instant::now();
        let mut t = Timings::default();
        for i in 0..100 {
            t.record(start + Duration::from_millis(i * 30), 40.0);
        }
        // Then a stretch of quick ones that fills the window
        for i in 100..300 {
            t.record(start + Duration::from_millis(i * 30), 5.0);
        }
        assert!(t.slow(start + Duration::from_millis(9000), 33.3).is_none());
        assert_eq!(t.recent().unwrap().max_ms, 5.0);
    }
}
