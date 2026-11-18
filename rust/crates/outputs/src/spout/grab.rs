//! Which way a reader takes a picture off the card (BGRA for the CPU to convert, or I420 planes the card made), kept apart from Direct3D so every system checks it. Also how it keeps the balance going while pictures stay off the card, and what it remembers of a card that failed.

use super::Picture;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use telescope_core::balance::{Balance, Place};

/// What a reader can take a picture off the card with.
pub trait Grabber {
    /// The picture as BGRA bytes, the way it was always read: Ok(None) while the sender holds its texture.
    fn bgra(&mut self) -> Result<Option<Picture>, String>;
    /// The picture as I420 planes the card made, with how long it waited for the card; a frame the card can't make comes as BGRA with no wait (or with the wait, when the card was too slow), never broken.
    fn card(&mut self) -> Result<Option<(Picture, Option<Duration>)>, String>;
    /// A frame's worth of conversion on the card with the result thrown away, how long it waited for the card; None when it couldn't be done in a short time.
    fn trial(&mut self) -> Option<Duration>;
}

/// A trial out on the card, however it ends: a panic still lets the next one go.
struct Trial<'a>(&'a Balance, Option<u64>);

impl Drop for Trial<'_> {
    fn drop(&mut self) {
        self.0.trial_done(self.1);
    }
}

fn nanos(wait: Duration) -> u64 {
    u64::try_from(wait.as_nanos()).unwrap_or(u64::MAX)
}

/// The next picture off the card the way `balance` says for this frame; it is asked once, and what the card made a frame wait is told back. While pictures stay off the card, nothing else may measure it for this reader, so it runs the trials the balance asks for.
pub fn grab(balance: &Balance, grabber: &mut impl Grabber) -> Result<Option<Picture>, String> {
    match balance.place() {
        Place::Cpu => {
            if balance.trial_due() {
                let mut trial = Trial(balance, None);
                trial.1 = grabber.trial().map(nanos);
            }
            grabber.bgra()
        }
        Place::Card => {
            let got = grabber.card()?;
            Ok(got.map(|(picture, wait)| {
                if let Some(wait) = wait {
                    balance.note_wait(nanos(wait));
                }
                picture
            }))
        }
    }
}

/// How long a failure of the card's conversion is remembered for a sender: long enough to cover the reopening a reset forces, short enough that a later session gets a fresh try.
const TROUBLE_KEPT: Duration = Duration::from_secs(300);

/// How many frames in a row the card's conversion failed on, per sender, kept beyond any one reader: a failure that ends a reader (the device was reset) would otherwise be forgotten with it and the next reader would run into the same fault.
#[derive(Default)]
pub struct Troubles(Mutex<HashMap<String, (u32, Instant)>>);

impl Troubles {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, (u32, Instant)>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The failures in a row remembered for the sender, none once the last was long enough ago.
    pub fn fails(&self, key: &str, now: Instant) -> u32 {
        self.map().get(key).filter(|(_, at)| now.saturating_duration_since(*at) < TROUBLE_KEPT).map_or(0, |(n, _)| *n)
    }

    /// One more failure for the sender; the count now.
    pub fn failed(&self, key: &str, now: Instant) -> u32 {
        let fails = self.fails(key, now) + 1;
        let mut map = self.map();
        map.retain(|_, (_, at)| now.saturating_duration_since(*at) < TROUBLE_KEPT);
        map.insert(key.to_string(), (fails, now));
        fails
    }

    /// A frame came out right: the run of failures is over.
    pub fn worked(&self, key: &str) {
        self.map().remove(key);
    }
}

/// The balance for each graphics adapter, so a card that is busy with one thing doesn't move what runs on another: a laptop's integrated chip reading a sender must not push what a discrete one does onto the CPU, nor the trials of one bring the work back to the other.
#[derive(Default)]
pub struct Balances(Mutex<HashMap<u64, Arc<Balance>>>);

impl Balances {
    /// The balance of the adapter with this id. One that is pinned (TELESCOPE_FRAME_STEPS) is pinned for every adapter, so `shared` stands for all of them then.
    pub fn of(&self, luid: u64, shared: &Arc<Balance>) -> Arc<Balance> {
        if shared.is_pinned() {
            return shared.clone();
        }
        self.0.lock().unwrap_or_else(|e| e.into_inner()).entry(luid).or_insert_with(|| Arc::new(Balance::free())).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::super::I420;
    use super::*;

    #[derive(Default)]
    struct Mock {
        bgra: u32,
        card: u32,
        /// What the card gives next: a picture with its wait, or a frame it fell back on.
        wait: Option<Duration>,
        busy: bool,
        fail: bool,
        trials: u32,
        /// What a trial waited, or None for one that couldn't be done.
        trial_wait: Option<Duration>,
    }

    fn pic(fourcc: &'static str) -> Picture {
        Picture { width: 2, height: 2, fourcc, data: Vec::new() }
    }

    impl Grabber for Mock {
        fn bgra(&mut self) -> Result<Option<Picture>, String> {
            self.bgra += 1;
            Ok((!self.busy).then(|| pic("BGRA")))
        }
        fn card(&mut self) -> Result<Option<(Picture, Option<Duration>)>, String> {
            self.card += 1;
            if self.fail {
                return Err("gone".into());
            }
            if self.busy {
                return Ok(None);
            }
            Ok(Some((pic(self.wait.map_or("BGRA", |_| I420)), self.wait)))
        }
        fn trial(&mut self) -> Option<Duration> {
            self.trials += 1;
            self.trial_wait
        }
    }

    #[test]
    fn the_cpu_takes_the_bgra_way_and_the_card_is_never_asked() {
        let balance = Balance::pinned_to(Place::Cpu);
        let mut mock = Mock { wait: Some(Duration::from_millis(1)), ..Mock::default() };
        let picture = grab(&balance, &mut mock).unwrap().unwrap();
        assert_eq!((picture.fourcc, mock.bgra, mock.card), ("BGRA", 1, 0));
        assert_eq!(balance.last_wait(), 0);
    }

    #[test]
    fn the_card_gives_planes_and_its_wait_is_told_back() {
        let balance = Balance::pinned_to(Place::Card);
        let mut mock = Mock { wait: Some(Duration::from_micros(2500)), ..Mock::default() };
        let picture = grab(&balance, &mut mock).unwrap().unwrap();
        assert_eq!((picture.fourcc, mock.bgra, mock.card), (I420, 0, 1));
        assert_eq!(balance.last_wait(), 2_500_000);
    }

    #[test]
    fn a_frame_the_card_fell_back_on_comes_as_bgra_and_tells_no_wait() {
        let balance = Balance::pinned_to(Place::Card);
        let mut mock = Mock::default();
        let picture = grab(&balance, &mut mock).unwrap().unwrap();
        assert_eq!(picture.fourcc, "BGRA");
        assert_eq!(balance.last_wait(), 0);
    }

    #[test]
    fn a_sender_holding_its_texture_gives_nothing_either_way() {
        for place in [Place::Cpu, Place::Card] {
            let mut mock = Mock { busy: true, wait: Some(Duration::from_millis(9)), ..Mock::default() };
            let balance = Balance::pinned_to(place);
            assert!(grab(&balance, &mut mock).unwrap().is_none());
            assert_eq!(balance.last_wait(), 0);
        }
    }

    #[test]
    fn an_error_from_the_reader_reaches_the_caller() {
        let mut mock = Mock { fail: true, ..Mock::default() };
        assert_eq!(grab(&Balance::pinned_to(Place::Card), &mut mock).unwrap_err(), "gone");
    }

    #[test]
    fn late_waits_move_the_grabs_off_the_card_one_ask_a_grab() {
        let balance = Balance::free();
        let mut mock = Mock { wait: Some(Duration::from_millis(30)), ..Mock::default() };
        for _ in 0..40 {
            let before = mock.card;
            grab(&balance, &mut mock).unwrap();
            assert!(mock.card - before <= 1, "asked the card twice in one grab");
        }
        // The fourth late wait of the last 16 left the card, and the rest of the grabs went the BGRA way
        assert_eq!((balance.place(), mock.card, mock.bgra), (Place::Cpu, 4, 36));
    }

    /// A balance that has left the card, with its first trial due.
    fn off_the_card() -> Balance {
        let balance = Balance::free();
        (0..4).for_each(|_| balance.note_wait(25_000_000));
        assert_eq!(balance.place(), Place::Cpu);
        // The first trial comes TRIAL_EVERY (half a second) after the move
        std::thread::sleep(Duration::from_millis(520));
        balance
    }

    #[test]
    fn off_the_card_a_trial_runs_when_the_balance_asks_and_its_wait_is_told_back() {
        let balance = off_the_card();
        let mut mock = Mock { trial_wait: Some(Duration::from_millis(2)), ..Mock::default() };
        let picture = grab(&balance, &mut mock).unwrap().unwrap();
        assert_eq!((picture.fourcc, mock.trials, mock.bgra, mock.card), ("BGRA", 1, 1, 0));
        assert_eq!(balance.last_wait(), 2_000_000);
        // The next one isn't due yet, and the frames go on the BGRA way
        grab(&balance, &mut mock).unwrap();
        assert_eq!((mock.trials, mock.bgra), (1, 2));
    }

    #[test]
    fn a_trial_that_couldnt_be_done_is_told_as_failed_so_the_next_can_go() {
        let balance = off_the_card();
        let mut mock = Mock::default();
        grab(&balance, &mut mock).unwrap();
        assert_eq!((mock.trials, balance.last_wait()), (1, u64::MAX));
        std::thread::sleep(Duration::from_millis(520));
        grab(&balance, &mut mock).unwrap();
        assert_eq!(mock.trials, 2, "the failed one was ended, so the next could be asked");
    }

    #[test]
    fn a_trial_that_panics_still_ends() {
        struct Panics;
        impl Grabber for Panics {
            fn bgra(&mut self) -> Result<Option<Picture>, String> {
                Ok(None)
            }
            fn card(&mut self) -> Result<Option<(Picture, Option<Duration>)>, String> {
                Ok(None)
            }
            fn trial(&mut self) -> Option<Duration> {
                panic!("boom")
            }
        }
        let balance = off_the_card();
        assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| grab(&balance, &mut Panics))).is_err());
        std::thread::sleep(Duration::from_millis(520));
        assert!(balance.trial_due(), "the trial that panicked must not stay out");
    }

    #[test]
    fn on_the_card_or_pinned_no_trial_runs() {
        let mut mock = Mock { wait: Some(Duration::from_millis(1)), trial_wait: Some(Duration::from_millis(1)), ..Mock::default() };
        grab(&Balance::free(), &mut mock).unwrap();
        grab(&Balance::pinned_to(Place::Cpu), &mut mock).unwrap();
        assert_eq!(mock.trials, 0);
    }

    #[test]
    fn a_failure_is_remembered_beyond_a_reader_and_a_good_frame_ends_the_run() {
        let troubles = Troubles::default();
        let now = Instant::now();
        assert_eq!(troubles.fails("Cam", now), 0);
        assert_eq!((troubles.failed("Cam", now), troubles.failed("Cam", now)), (1, 2));
        assert_eq!((troubles.fails("Cam", now), troubles.fails("Other", now)), (2, 0));
        troubles.worked("Cam");
        assert_eq!(troubles.fails("Cam", now), 0);
    }

    #[test]
    fn a_failure_is_forgotten_after_a_while() {
        let troubles = Troubles::default();
        let now = Instant::now();
        troubles.failed("Cam", now);
        troubles.failed("Cam", now);
        assert_eq!(troubles.fails("Cam", now + TROUBLE_KEPT - Duration::from_secs(1)), 2);
        assert_eq!(troubles.fails("Cam", now + TROUBLE_KEPT), 0);
        assert_eq!(troubles.failed("Cam", now + TROUBLE_KEPT), 1, "a new run starts over");
    }

    #[test]
    fn each_adapter_has_its_own_balance_and_the_same_adapter_the_same_one() {
        let balances = Balances::default();
        let shared = Arc::new(Balance::free());
        let (a, b) = (balances.of(1, &shared), balances.of(2, &shared));
        assert!(!Arc::ptr_eq(&a, &b) && !Arc::ptr_eq(&a, &shared));
        assert!(Arc::ptr_eq(&a, &balances.of(1, &shared)));
        // One adapter's slow card leaves the other's balance alone
        (0..4).for_each(|_| a.note_wait(25_000_000));
        assert_eq!((a.place(), b.place()), (Place::Cpu, Place::Card));
    }

    #[test]
    fn a_pinned_balance_stands_for_every_adapter() {
        let balances = Balances::default();
        let shared = Arc::new(Balance::pinned_to(Place::Cpu));
        assert!(Arc::ptr_eq(&balances.of(1, &shared), &shared) && Arc::ptr_eq(&balances.of(2, &shared), &shared));
    }
}
