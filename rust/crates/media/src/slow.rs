//! Whether a decoder keeps up with the stream: how long each picture takes to decode and be handed over (on a card, until it is copied off or
//! kept there), judged over a few seconds. A card that looks too slow hands over to software only if software turns out quicker and also
//! gets more new pictures to the virtual camera.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

#[cfg(test)]
thread_local! {
    static SKEW: std::cell::Cell<Duration> = const { std::cell::Cell::new(Duration::ZERO) };
}

/// The time for the decode speed check: the real clock, which tests move forward by hand.
pub fn now() -> Instant {
    #[cfg(test)]
    return Instant::now() + SKEW.with(|s| s.get());
    #[cfg(not(test))]
    Instant::now()
}

/// Tests: moves the speed check's clock forward, as if that long had passed.
#[cfg(test)]
pub fn advance(by: Duration) {
    SKEW.with(|s| s.set(s.get() + by));
}

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

/// Software must take less than this share of the card's time a picture to be called quicker: a tie goes to the card, which costs the processor less.
pub const CLEARLY_QUICKER: f64 = 0.8;
/// How long software gets to show its speed once it has the stream, before the card is taken back anyway.
pub const TRIAL_LIMIT: Duration = Duration::from_secs(10);
/// How long a decoder started beside the running one gets to give its first picture (it starts at the next keyframe) before it is given up on.
pub const START_LIMIT: Duration = Duration::from_secs(8);
/// Software must deliver this much more than the card, new pictures a second, to be called better: the same rate is a tie, and a tie goes to the card.
pub const RATE_MARGIN: f64 = 1.05;

/// Whether software, at software_ms a picture, is clearly quicker than a card at card_ms.
pub fn software_quicker(card_ms: f64, software_ms: f64) -> bool {
    software_ms < card_ms * CLEARLY_QUICKER
}

/// Whether software's new pictures a second beat the card's by the margin; a rate that couldn't be measured beats nothing.
pub fn delivers_more(card: Option<f64>, software: Option<f64>) -> bool {
    matches!((card, software), (Some(card), Some(software)) if software > 0.0 && software >= card * RATE_MARGIN)
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

/// A rate older than this has nothing to say about now.
const FRESH: Duration = Duration::from_secs(1);
/// The least time a rate is read over.
const MIN_SPAN: Duration = Duration::from_secs(2);

/// How many new pictures a second reached the virtual camera, read from a running total over the last WINDOW.
#[derive(Default)]
pub struct Rate {
    samples: VecDeque<(Instant, u64)>,
}

impl Rate {
    /// The running total of new pictures sent on as of now.
    pub fn record(&mut self, now: Instant, total: u64) {
        // A total that went back is another stream's count: start over
        if self.samples.back().is_some_and(|&(_, last)| total < last) {
            self.samples.clear();
        }
        self.samples.push_back((now, total));
        // Keeps one sample at or before the start of the window
        while self.samples.len() >= 2 && now.duration_since(self.samples[1].0) >= WINDOW {
            self.samples.pop_front();
        }
    }

    /// New pictures a second over the last window, or None when the newest sample is stale or they span too little time to say.
    pub fn per_second(&self, now: Instant) -> Option<f64> {
        let (&(first_at, first), &(last_at, last)) = (self.samples.front()?, self.samples.back()?);
        let span = last_at.duration_since(first_at);
        if now.saturating_duration_since(last_at) > FRESH || span < MIN_SPAN {
            return None;
        }
        Some((last - first) as f64 / span.as_secs_f64())
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
        too_slow(&self.times_at(now), interval_ms)
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

    /// The median picture of the last window once a whole window was timed and it holds enough pictures to say; None before that. Pictures
    /// older than the window are left out even when no newer one came to push them off.
    pub fn per_picture(&self, now: Instant) -> Option<f64> {
        if now.duration_since(self.since?) < WINDOW {
            return None;
        }
        let times = self.times_at(now);
        if times.len() < MIN_PICTURES {
            return None;
        }
        speed(&times).map(|s| s.median_ms)
    }

    fn times(&self) -> Vec<f64> {
        self.recent.iter().map(|(_, ms)| *ms).collect()
    }

    /// The times of the pictures within the last window as of now.
    fn times_at(&self, now: Instant) -> Vec<f64> {
        self.recent.iter().filter(|(t, _)| now.saturating_duration_since(*t) <= WINDOW).map(|(_, ms)| *ms).collect()
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

    #[test]
    fn per_picture_waits_for_a_whole_window() {
        let start = Instant::now();
        let mut t = Timings::default();
        for i in 0..60 {
            t.record(start + Duration::from_millis(i * 33), 20.0);
        }
        assert!(t.per_picture(start + Duration::from_secs(2)).is_none());
        assert_eq!(t.per_picture(start + Duration::from_secs(3)), Some(20.0));
    }

    #[test]
    fn per_picture_needs_enough_pictures_to_say() {
        let start = Instant::now();
        let mut t = Timings::default();
        // One quick picture and then nothing: that is no verdict, let alone a quick one
        t.record(start, 2.0);
        assert!(t.per_picture(start + Duration::from_secs(3)).is_none());
        for i in 0..MIN_PICTURES as u64 - 2 {
            t.record(start + Duration::from_millis(100 * i), 2.0);
        }
        assert!(t.per_picture(start + Duration::from_secs(3)).is_none());
        t.record(start + Duration::from_millis(2900), 2.0);
        assert_eq!(t.per_picture(start + Duration::from_secs(3)), Some(2.0));
    }

    #[test]
    fn pictures_that_were_never_pushed_off_still_expire() {
        let start = Instant::now();
        let mut t = Timings::default();
        for i in 0..100 {
            t.record(start + Duration::from_millis(i * 30), 5.0);
        }
        // Nothing came after, so nothing evicted them: they are old all the same
        assert!(t.per_picture(start + Duration::from_secs(3)).is_some());
        assert!(t.per_picture(start + Duration::from_secs(9)).is_none());
        assert!(t.slow(start + Duration::from_secs(9), 1.0).is_none());
    }

    #[test]
    fn the_rate_reads_the_last_window_of_a_running_total() {
        let start = Instant::now();
        let mut r = Rate::default();
        for i in 0..=150u64 {
            // 20 a second for the first three seconds, then 10 a second
            let total = if i <= 60 { i } else { 60 + (i - 60) / 2 };
            r.record(start + Duration::from_millis(i * 50), total);
        }
        let rate = r.per_second(start + Duration::from_millis(7500)).unwrap();
        assert!((rate - 10.0).abs() < 0.5, "{rate}");
    }

    #[test]
    fn a_rate_needs_time_and_a_fresh_sample() {
        let start = Instant::now();
        let mut r = Rate::default();
        assert!(r.per_second(start).is_none());
        r.record(start, 0);
        r.record(start + Duration::from_millis(1000), 30);
        assert!(r.per_second(start + Duration::from_millis(1000)).is_none(), "one second says little");
        r.record(start + Duration::from_millis(2500), 75);
        assert_eq!(r.per_second(start + Duration::from_millis(2500)), Some(30.0));
        assert!(r.per_second(start + Duration::from_millis(3800)).is_none(), "the last sample is stale");
    }

    #[test]
    fn a_total_that_goes_back_starts_the_rate_over() {
        let start = Instant::now();
        let mut r = Rate::default();
        r.record(start, 100);
        r.record(start + Duration::from_secs(3), 200);
        r.record(start + Duration::from_millis(3100), 5);
        assert!(r.per_second(start + Duration::from_millis(3100)).is_none());
    }

    #[test]
    fn software_has_to_deliver_clearly_more_pictures_than_the_card() {
        assert!(delivers_more(Some(15.0), Some(25.0)));
        assert!(delivers_more(Some(0.0), Some(5.0)));
        assert!(!delivers_more(Some(25.0), Some(15.0)));
        assert!(!delivers_more(Some(25.0), Some(25.5)), "inside the margin is a tie");
        assert!(!delivers_more(Some(0.0), Some(0.0)));
        assert!(!delivers_more(None, Some(30.0)) && !delivers_more(Some(10.0), None));
    }

    #[test]
    fn software_has_to_be_clearly_quicker_than_the_card() {
        assert!(software_quicker(45.0, 20.0));
        assert!(!software_quicker(30.0, 26.0));
        assert!(!software_quicker(30.0, 75.0));
        assert!(!software_quicker(30.0, f64::INFINITY));
    }
}
