//! Which way a reader takes a picture off the card (BGRA for the CPU to convert, or I420 planes the card made), kept apart from Direct3D so every system checks it.

use super::Picture;
use std::time::Duration;
use telescope_core::balance::{Balance, Place};

/// What a reader can take a picture off the card with.
pub trait Grabber {
    /// The picture as BGRA bytes, the way it was always read: Ok(None) while the sender holds its texture.
    fn bgra(&mut self) -> Result<Option<Picture>, String>;
    /// The picture as I420 planes the card made, with how long it waited for the card; a frame the card can't make comes as BGRA with no wait, never broken.
    fn card(&mut self) -> Result<Option<(Picture, Option<Duration>)>, String>;
}

/// The next picture off the card the way `balance` says for this frame; it is asked once, and what the card made a frame wait is told back.
pub fn grab(balance: &Balance, grabber: &mut impl Grabber) -> Result<Option<Picture>, String> {
    match balance.place() {
        Place::Cpu => grabber.bgra(),
        Place::Card => {
            let got = grabber.card()?;
            Ok(got.map(|(picture, wait)| {
                if let Some(wait) = wait {
                    balance.note_wait(u64::try_from(wait.as_nanos()).unwrap_or(u64::MAX));
                }
                picture
            }))
        }
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
}
