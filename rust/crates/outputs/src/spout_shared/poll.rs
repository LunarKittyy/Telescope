//! Waiting for a result by asking again until it's there, the caller has said stop, or the time is up. Plain code, kept apart from Direct3D so every system checks it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// How long a wait keeps yielding the thread between asks before it starts sleeping; most waits end well inside it, and a sleep can overshoot by a whole timer tick, which would be counted as the card's lateness.
const SPIN: Duration = Duration::from_millis(10);
const NAP: Duration = Duration::from_millis(1);

/// When a wait has to give up: at a time, or as soon as the stop flag is set.
pub struct Deadline {
    until: Instant,
    stop: Option<Arc<AtomicBool>>,
}

impl Deadline {
    /// A wait of at most `within` that also ends when `stop` is set.
    pub fn new(stop: &Arc<AtomicBool>, within: Duration) -> Deadline {
        Deadline { until: Instant::now() + within, stop: Some(stop.clone()) }
    }

    /// A wait of at most `within` that nothing else ends.
    #[cfg(test)]
    pub fn within(within: Duration) -> Deadline {
        Deadline { until: Instant::now() + within, stop: None }
    }

    fn stopped(&self) -> bool {
        self.stop.as_ref().is_some_and(|stop| stop.load(Ordering::SeqCst))
    }
}

/// How a wait ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Polled<T> {
    Ready(T),
    Cancelled,
    TimedOut,
}

/// Asks `attempt` until it gives Some, always asking at least once so a result that is ready is taken even by a wait that was stopped already.
pub fn poll<T>(deadline: &Deadline, mut attempt: impl FnMut() -> Option<T>) -> Polled<T> {
    let started = Instant::now();
    loop {
        if let Some(ready) = attempt() {
            return Polled::Ready(ready);
        }
        if deadline.stopped() {
            return Polled::Cancelled;
        }
        if Instant::now() >= deadline.until {
            return Polled::TimedOut;
        }
        if started.elapsed() < SPIN {
            std::thread::yield_now();
        } else {
            std::thread::sleep(NAP);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_result_that_is_ready_is_taken_at_once_even_when_stopped() {
        let stop = Arc::new(AtomicBool::new(true));
        assert_eq!(poll(&Deadline::new(&stop, Duration::ZERO), || Some(7)), Polled::Ready(7));
    }

    #[test]
    fn it_asks_again_until_the_result_comes() {
        let mut asked = 0;
        let got = poll(&Deadline::within(Duration::from_secs(10)), || {
            asked += 1;
            (asked == 40).then_some(asked)
        });
        assert_eq!(got, Polled::Ready(40));
    }

    #[test]
    fn a_stop_ends_the_wait_without_waiting_out_the_time() {
        let stop = Arc::new(AtomicBool::new(false));
        let started = Instant::now();
        let mut asked = 0;
        let got = poll(&Deadline::new(&stop, Duration::from_secs(60)), || {
            asked += 1;
            if asked == 5 {
                stop.store(true, Ordering::SeqCst);
            }
            None::<()>
        });
        assert_eq!(got, Polled::Cancelled);
        assert_eq!(asked, 5);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_result_that_never_comes_times_out_after_the_time_given() {
        let started = Instant::now();
        assert_eq!(poll(&Deadline::within(Duration::from_millis(30)), || None::<()>), Polled::TimedOut);
        assert!(started.elapsed() >= Duration::from_millis(30));
    }
}
