//! Measuring a phone's clock offset from round trips to its /v1/ping while a stream runs, so the capture times the phone stamps on its frames
//! can be read on this computer's clock (core::clock). The decoder of the stream only reads what this keeps up to date.

use crate::connection::with_session;
use crate::https::PhoneAuth;
use crate::phones::UsbTunnels;
use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::clock::{ClockOffset, PhoneClock};
use telescope_core::source::RouteSpec;

/// How many round trips the estimate looks back over.
pub const KEPT: usize = 8;
/// Round trips in a quick burst when a stream starts, so there's an offset within the first second.
pub const BURST: usize = 5;
const BURST_GAP: Duration = Duration::from_millis(100);
/// Round trips after the burst come this far apart.
pub const EVERY: Duration = Duration::from_secs(2);
/// Another try at reaching the phone when a ping got no answer.
const RETRY: Duration = Duration::from_secs(2);

/// What a phone's clocks said over one round trip: the request went out at `sent_ns` and the answer came in at `got_ns` (this computer's
/// monotonic nanoseconds), and the phone read its clocks somewhere between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sample {
    pub sent_ns: u64,
    pub got_ns: u64,
    pub boot_ns: Option<i64>,
    pub mono_ns: Option<i64>,
}

impl Sample {
    pub fn round_trip_ns(&self) -> u64 {
        self.got_ns.saturating_sub(self.sent_ns)
    }

    /// The offset this one round trip says: the phone's value minus the middle of the round trip, off by at most half of it. None when the phone
    /// gave no clock.
    pub fn offset(&self) -> Option<ClockOffset> {
        if self.boot_ns.is_none() && self.mono_ns.is_none() {
            return None;
        }
        let middle = (i128::from(self.sent_ns) + i128::from(self.got_ns)) / 2;
        let against = |phone: Option<i64>| phone.map(|p| (i128::from(p) - middle).clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64);
        Some(ClockOffset { boot_ns: against(self.boot_ns), mono_ns: against(self.mono_ns), error_ns: self.round_trip_ns() / 2 })
    }
}

/// Round trips older than this (against the newest one) are forgotten, so a long sleep of this computer can't leave an old sample winning.
const MAX_AGE_NS: u64 = 20_000_000_000;
/// Without a successful ping for this long the clock is cleared, so nothing reports a bound that isn't known to hold anymore.
const STALE_AFTER: Duration = Duration::from_secs(10);
/// A new offset this far from the best one (besides ten times its own error) means a clock jumped, and the old samples are dropped.
const JUMP_NS: u64 = 50_000_000;

/// The best offset of the last few round trips: the one with the shortest round trip, since its error bound is the smallest (a slow answer
/// says as much about the network as about the phone).
#[derive(Debug, Default)]
pub struct Estimator {
    samples: VecDeque<Sample>,
}

/// Whether two offsets disagree by more than the new one's error allows (on the first clock both tell).
fn jumped(best: &ClockOffset, new: &ClockOffset) -> bool {
    let limit = new.error_ns.saturating_mul(10).max(JUMP_NS);
    [(best.boot_ns, new.boot_ns), (best.mono_ns, new.mono_ns)]
        .into_iter()
        .find_map(|(a, b)| Some(a?.abs_diff(b?)))
        .is_some_and(|d| d > limit)
}

impl Estimator {
    /// Takes in a round trip and gives the best offset of the last KEPT within MAX_AGE_NS, or None while no round trip had a phone clock in it
    /// (an older phone). An offset that jumps away from the best one starts the estimate over from the new one.
    pub fn add(&mut self, sample: Sample) -> Option<ClockOffset> {
        if let Some(new) = sample.offset() {
            if self.best().is_some_and(|best| jumped(&best, &new)) {
                self.samples.clear();
            }
            self.samples.retain(|s| sample.got_ns.saturating_sub(s.got_ns) <= MAX_AGE_NS);
            if self.samples.len() == KEPT {
                self.samples.pop_front();
            }
            self.samples.push_back(sample);
        }
        self.best()
    }

    /// The best offset so far; with equal round trips, the newer one's.
    pub fn best(&self) -> Option<ClockOffset> {
        self.samples.iter().rev().min_by_key(|s| s.round_trip_ns()).and_then(Sample::offset)
    }
}

/// How long to wait after the nth round trip (counted from 1) before the next.
fn gap(taken: usize) -> Duration {
    if taken < BURST { BURST_GAP } else { EVERY }
}

/// Pings through ping until stop says to end (a message, or its sender gone), keeping clock up to date. Without an answer for stale_after the clock
/// is cleared, and a ping that got none is tried again after retry.
fn run(ping: &mut dyn FnMut() -> Option<Sample>, clock: &PhoneClock, estimator: &mut Estimator, stop: &mpsc::Receiver<()>,
       retry: Duration, stale_after: Duration) {
    let (mut taken, mut last_ok) = (0, Instant::now());
    loop {
        let wait = match ping() {
            Some(sample) => {
                last_ok = Instant::now();
                if let Some(offset) = estimator.add(sample) {
                    clock.set(offset);
                }
                taken += 1;
                gap(taken)
            }
            None => {
                if last_ok.elapsed() > stale_after {
                    clock.clear();
                }
                retry
            }
        };
        if !matches!(stop.recv_timeout(wait), Err(mpsc::RecvTimeoutError::Timeout)) {
            return;
        }
    }
}

/// A phone's clock offset, measured on a thread of its own from the moment it's made until it's stopped or let go.
pub struct Syncer {
    clock: Arc<PhoneClock>,
    stop: Mutex<Option<mpsc::Sender<()>>>,
}

impl Syncer {
    /// Starts measuring the phone at route. Each ping opens its own session (and so its own USB forward, held only for that ping), the way other
    /// periodic calls do, so a replugged cable is found again.
    pub fn start(auth: PhoneAuth, route: RouteSpec, tunnels: Arc<UsbTunnels>) -> Syncer {
        let clock = Arc::new(PhoneClock::default());
        let (tx, rx) = mpsc::channel::<()>();
        let (shared, mine) = (clock.clone(), Syncer { clock: clock.clone(), stop: Mutex::new(Some(tx)) });
        let spawned = std::thread::Builder::new().name("clock-sync".into()).spawn(move || {
            let mut ping = || {
                with_session(Some(&auth), Some(&route), &*tunnels, |client| client.and_then(|c| c.ping().clocks))
                    .map(|c| Sample { sent_ns: c.sent_ns, got_ns: c.got_ns, boot_ns: c.boot_ns, mono_ns: c.mono_ns })
            };
            run(&mut ping, &shared, &mut Estimator::default(), &rx, RETRY, STALE_AFTER);
        });
        if let Err(e) = spawned {
            log::warn!("Couldn't start measuring the phone's clock: {e}");
        }
        mine
    }

    /// The offset it keeps up to date.
    pub fn clock(&self) -> Arc<PhoneClock> {
        self.clock.clone()
    }

    /// Ends the measuring; the last offset stays in the clock.
    pub fn stop(&self) {
        self.stop.lock().unwrap_or_else(|e| e.into_inner()).take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::https::testing::serve;
    use crate::session_client::SessionClient;

    fn sample(sent: u64, got: u64, boot: i64) -> Sample {
        Sample { sent_ns: sent, got_ns: got, boot_ns: Some(boot), mono_ns: None }
    }

    #[test]
    fn one_round_trip_puts_the_phones_time_at_its_middle() {
        // Sent at 1000, back at 1400: the phone read 5200 when this computer's clock said about 1200
        let o = sample(1000, 1400, 5200).offset().unwrap();
        assert_eq!((o.boot_ns, o.mono_ns, o.error_ns), (Some(4000), None, 200));
    }

    #[test]
    fn the_round_trip_with_the_smallest_error_wins() {
        let mut e = Estimator::default();
        // A slow one first, then a quick one that disagrees by less than the slow one's error, then another slow one
        assert_eq!(e.add(sample(0, 10_000, 105_000)).unwrap().error_ns, 5_000);
        let best = e.add(sample(20_000, 20_400, 120_300)).unwrap();
        assert_eq!((best.boot_ns, best.error_ns), (Some(100_100), 200));
        let still = e.add(sample(40_000, 49_000, 144_000)).unwrap();
        assert_eq!(still, best, "a slower round trip doesn't replace a better one");
    }

    #[test]
    fn with_equal_round_trips_the_newer_one_counts() {
        let mut e = Estimator::default();
        e.add(sample(0, 1_000, 100_500));
        let newer = e.add(sample(10_000, 11_000, 110_700)).unwrap();
        assert_eq!(newer.boot_ns, Some(100_200));
    }

    #[test]
    fn only_the_last_eight_are_looked_at() {
        let mut e = Estimator::default();
        // The best one comes first and ages out after eight more
        assert_eq!(e.add(sample(0, 100, 1_050)).unwrap().error_ns, 50);
        for i in 1..KEPT as u64 {
            assert_eq!(e.add(sample(i * 10_000, i * 10_000 + 1_000, 1_500 + i as i64 * 10_000)).unwrap().error_ns, 50, "still there after {i}");
        }
        let after = e.add(sample(90_000, 91_000, 91_500)).unwrap();
        assert_eq!(after.error_ns, 500, "the best one is gone");
    }

    #[test]
    fn a_phone_with_no_clock_never_gives_an_offset() {
        let mut e = Estimator::default();
        assert_eq!(e.add(Sample { sent_ns: 0, got_ns: 100, boot_ns: None, mono_ns: None }), None);
        assert_eq!(e.best(), None);
        // Nor does one that comes after leave a hole: the clocks the phone did give count
        let both = e.add(Sample { sent_ns: 0, got_ns: 100, boot_ns: Some(60), mono_ns: Some(-40) }).unwrap();
        assert_eq!((both.boot_ns, both.mono_ns), (Some(10), Some(-90)));
    }

    #[test]
    fn the_offset_is_found_whichever_way_the_clocks_differ() {
        // The phone's clock is behind this computer's by a lot, and ahead by a lot
        for offset in [-9_000_000_000_000i64, 0, 5_000_000_000_000] {
            let sent = 2_000_000_000_000u64;
            let phone = (sent as i64 + 500) + offset;
            let o = sample(sent, sent + 1_000, phone).offset().unwrap();
            assert_eq!(o.boot_ns, Some(offset));
        }
    }

    #[test]
    fn it_asks_in_a_burst_and_then_every_two_seconds() {
        let gaps: Vec<_> = (1..=7).map(gap).collect();
        assert_eq!(gaps[..BURST - 1], [BURST_GAP; BURST - 1]);
        assert!(gaps[BURST - 1..].iter().all(|g| *g == EVERY));
    }

    #[test]
    fn samples_older_than_twenty_seconds_are_forgotten() {
        let mut e = Estimator::default();
        e.add(sample(0, 100, 1_050));
        // 25 s later the old, better round trip is gone
        let at = 25_000_000_000u64;
        let after = e.add(sample(at, at + 1_000, at as i64 + 1_500)).unwrap();
        assert_eq!(after.error_ns, 500);
    }

    #[test]
    fn a_jump_in_the_offset_starts_over_from_the_new_sample() {
        let mut e = Estimator::default();
        e.add(sample(0, 100, 1_050));
        // The phone's clock is 1 s ahead of what it was (a suspend): a slower round trip still replaces the old, better one
        let at = 2_000_000_000u64;
        let after = e.add(sample(at, at + 2_000, at as i64 + 1_000_001_000)).unwrap();
        assert_eq!((after.error_ns, after.boot_ns), (1_000, Some(1_000_000_000)));
        // A wobble of a few milliseconds is no jump, and the better sample stays
        let wobble = e.add(sample(at + 10_000, at + 14_000, at as i64 + 1_000_010_000 + 5_000_000)).unwrap();
        assert_eq!(wobble, after);
    }

    #[test]
    fn pings_that_keep_failing_clear_the_clock() {
        let (tx, rx) = mpsc::channel::<()>();
        let clock = Arc::new(PhoneClock::default());
        let mine = clock.clone();
        let mut answered = false;
        let mut ping = move || {
            let first = !std::mem::replace(&mut answered, true);
            first.then(|| sample(0, 100, 1_050))
        };
        let t = std::thread::spawn(move || run(&mut ping, &mine, &mut Estimator::default(), &rx, Duration::from_millis(10), Duration::from_millis(100)));
        let began = Instant::now();
        while clock.sync().is_none() && began.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(clock.sync().is_some(), "the answer sets it");
        while clock.sync().is_some() && began.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(clock.sync().is_none(), "no answers for a while clears it");
        drop(tx);
        t.join().unwrap();
    }

    #[test]
    fn a_phone_that_answers_gives_the_clock_an_offset_within_a_burst_and_stopping_ends_it() {
        let phone_now = |at: u64| 7_000_000_000_000i64 + at as i64;
        let server = serve(None, Arc::new(move |_req| {
            let body = format!(r#"{{"streaming": true, "clock_boot_ns": {}, "clock_mono_ns": {}}}"#, phone_now(telescope_core::time::monotonic_ns()), 1);
            (200, "application/json".into(), body.into_bytes())
        }));
        let client = SessionClient::new(&server.base(false), PhoneAuth::new("tok", ""));
        let mut ping = move || client.ping().clocks.map(|c| Sample { sent_ns: c.sent_ns, got_ns: c.got_ns, boot_ns: c.boot_ns, mono_ns: c.mono_ns });
        let (clock, (tx, rx)) = (Arc::new(PhoneClock::default()), mpsc::channel::<()>());
        let mine = clock.clone();
        let t = std::thread::spawn(move || run(&mut ping, &mine, &mut Estimator::default(), &rx, RETRY, STALE_AFTER));
        let began = Instant::now();
        while clock.offset().is_none() && began.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let offset = clock.offset().expect("the first answer gives an offset");
        // Both clocks were read at the same moment as this one's, so the offset is the 7000 s that was added, within the round trip
        assert!(offset.boot_ns.unwrap().abs_diff(7_000_000_000_000) <= offset.error_ns + 1_000_000, "{offset:?}");
        assert!(offset.error_ns < 1_000_000_000, "{offset:?}");
        // The burst keeps coming without waiting for the slow pace
        while server.seen.lock().unwrap().len() < BURST && began.elapsed() < Duration::from_secs(10) {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(server.seen.lock().unwrap().len() >= BURST, "a burst of {BURST}");
        assert!(began.elapsed() < EVERY * 2, "the burst isn't spread over two-second gaps");
        drop(tx);
        t.join().unwrap();
        let seen = server.seen.lock().unwrap().len();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(server.seen.lock().unwrap().len(), seen, "stopped means no more pings");
    }
}
