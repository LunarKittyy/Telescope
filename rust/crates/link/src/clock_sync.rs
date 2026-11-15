//! Measuring a phone's clock offset from round trips to its /v1/ping while a stream runs, so the capture times the phone stamps on its frames
//! can be read on this computer's clock (core::clock). The decoder of the stream only reads what this keeps up to date.

use crate::connection::with_session;
use crate::https::PhoneAuth;
use crate::phones::UsbTunnels;
use crate::session_client::SessionClient;
use std::collections::VecDeque;
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use telescope_core::clock::{ClockOffset, PhoneClock};
use telescope_core::source::RouteSpec;

/// How many round trips the estimate looks back over.
pub const KEPT: usize = 8;
/// Round trips in a quick burst when a stream starts, so there's an offset within the first second.
pub const BURST: usize = 5;
const BURST_GAP: Duration = Duration::from_millis(100);
/// Round trips after the burst come this far apart.
pub const EVERY: Duration = Duration::from_secs(2);
/// Another try at reaching the phone when its forward wouldn't open.
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

/// The best offset of the last few round trips: the one with the shortest round trip, since its error bound is the smallest (a slow answer
/// says as much about the network as about the phone).
#[derive(Debug, Default)]
pub struct Estimator {
    samples: VecDeque<Sample>,
}

impl Estimator {
    /// Takes in a round trip and gives the best offset of the last KEPT, or None while no round trip had a phone clock in it (an older phone).
    pub fn add(&mut self, sample: Sample) -> Option<ClockOffset> {
        if sample.offset().is_some() {
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

/// Pings through client until stop says to end (a message, or its sender gone), keeping clock up to date.
fn run(client: &SessionClient, clock: &PhoneClock, estimator: &mut Estimator, stop: &mpsc::Receiver<()>) {
    let mut taken = 0;
    loop {
        if let Some(c) = client.ping().clocks {
            if let Some(offset) = estimator.add(Sample { sent_ns: c.sent_ns, got_ns: c.got_ns, boot_ns: c.boot_ns, mono_ns: c.mono_ns }) {
                clock.set(offset);
            }
        }
        taken += 1;
        if !matches!(stop.recv_timeout(gap(taken)), Err(mpsc::RecvTimeoutError::Timeout)) {
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
    /// Starts measuring the phone at route (holding the forward to its session port over USB for as long as it runs).
    pub fn start(auth: PhoneAuth, route: RouteSpec, tunnels: Arc<UsbTunnels>) -> Syncer {
        let clock = Arc::new(PhoneClock::default());
        let (tx, rx) = mpsc::channel::<()>();
        let (shared, mine) = (clock.clone(), Syncer { clock: clock.clone(), stop: Mutex::new(Some(tx)) });
        let spawned = std::thread::Builder::new().name("clock-sync".into()).spawn(move || {
            let mut estimator = Estimator::default();
            loop {
                let ended = with_session(Some(&auth), Some(&route), &*tunnels, |client| match client {
                    Some(client) => {
                        run(client, &shared, &mut estimator, &rx);
                        true
                    }
                    // The forward didn't open: another go in a while
                    None => !matches!(rx.recv_timeout(RETRY), Err(mpsc::RecvTimeoutError::Timeout)),
                });
                if ended {
                    return;
                }
            }
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
    use std::time::Instant;

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
    fn a_phone_that_answers_gives_the_clock_an_offset_within_a_burst_and_stopping_ends_it() {
        let phone_now = |at: u64| 7_000_000_000_000i64 + at as i64;
        let server = serve(None, Arc::new(move |_req| {
            let body = format!(r#"{{"streaming": true, "clock_boot_ns": {}, "clock_mono_ns": {}}}"#, phone_now(telescope_core::time::monotonic_ns()), 1);
            (200, "application/json".into(), body.into_bytes())
        }));
        let client = SessionClient::new(&server.base(false), PhoneAuth::new("tok", ""));
        let (clock, (tx, rx)) = (Arc::new(PhoneClock::default()), mpsc::channel::<()>());
        let mine = clock.clone();
        let t = std::thread::spawn(move || run(&client, &mine, &mut Estimator::default(), &rx));
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
