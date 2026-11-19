//! How often pictures cross between the processor and the graphics card, the app over. A crossing costs far more than anything a
//! shader does, so later work and the stress test read this, and each stream's pipeline logs what a frame costs when that changes.

use std::sync::atomic::{AtomicU64, Ordering};

static FRAMES: AtomicU64 = AtomicU64::new(0);
static UP: AtomicU64 = AtomicU64::new(0);
static DOWN: AtomicU64 = AtomicU64::new(0);
static MASK_UP: AtomicU64 = AtomicU64::new(0);
static DECODED_DOWN: AtomicU64 = AtomicU64::new(0);
static PREVIEW_UP: AtomicU64 = AtomicU64::new(0);
static PREVIEW_DOWN: AtomicU64 = AtomicU64::new(0);

/// Totals since the app started.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Transfers {
    /// Frames through a pipeline.
    pub frames: u64,
    /// A frame step's picture going up to the card.
    pub up: u64,
    /// A frame step's picture coming back.
    pub down: u64,
    /// A person mask going up.
    pub mask_up: u64,
    /// A picture a card decoder made coming down.
    pub decoded_down: u64,
    /// A picture going up to be shown.
    pub preview_up: u64,
    /// A shown picture read back (a preview that isn't drawn straight on the card).
    pub preview_down: u64,
}

pub fn totals() -> Transfers {
    let read = |n: &AtomicU64| n.load(Ordering::Relaxed);
    Transfers {
        frames: read(&FRAMES),
        up: read(&UP),
        down: read(&DOWN),
        mask_up: read(&MASK_UP),
        decoded_down: read(&DECODED_DOWN),
        preview_up: read(&PREVIEW_UP),
        preview_down: read(&PREVIEW_DOWN),
    }
}

pub(crate) fn frame(up: u32, down: u32) {
    FRAMES.fetch_add(1, Ordering::Relaxed);
    UP.fetch_add(up as u64, Ordering::Relaxed);
    DOWN.fetch_add(down as u64, Ordering::Relaxed);
}

pub(crate) fn landed() {
    DOWN.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn mask_up() {
    MASK_UP.fetch_add(1, Ordering::Relaxed);
}

#[cfg_attr(not(ffmpeg), allow(dead_code))]
pub(crate) fn decoded_down() {
    DECODED_DOWN.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn preview_up() {
    PREVIEW_UP.fetch_add(1, Ordering::Relaxed);
}

pub(crate) fn preview_down() {
    PREVIEW_DOWN.fetch_add(1, Ordering::Relaxed);
}

#[cfg(test)]
pub(crate) mod measurement {
    use std::cell::Cell;
    use telescope_core::time::monotonic_ns;

    #[derive(Clone, Copy, Debug, Default)]
    pub struct Costs {
        pub transfer_ns: u64,
        pub lift_ns: u64,
        pub land_ns: u64,
        pub decoded_down: u32,
        pub up: u32,
        pub down: u32,
    }

    thread_local! {
        static CURRENT: Cell<Option<Costs>> = const { Cell::new(None) };
    }

    pub enum Stage { Transfer, Lift, Land }

    pub struct Timer(Stage, Option<u64>);

    pub fn time(stage: Stage) -> Timer {
        Timer(stage, CURRENT.with(|c| c.get().map(|_| monotonic_ns())))
    }

    impl Drop for Timer {
        fn drop(&mut self) {
            let Some(began) = self.1 else { return };
            CURRENT.with(|c| {
                if let Some(mut costs) = c.get() {
                    let ns = monotonic_ns().saturating_sub(began);
                    match self.0 {
                        Stage::Transfer => { costs.transfer_ns += ns; costs.decoded_down += 1; }
                        Stage::Lift => { costs.lift_ns += ns; costs.up += 1; }
                        Stage::Land => { costs.land_ns += ns; costs.down += 1; }
                    }
                    c.set(Some(costs));
                }
            });
        }
    }

    // Thread-local scopes keep other streams and parallel tests out of each frame's figures.
    pub fn measure<T>(run: impl FnOnce() -> T) -> (T, Costs) {
        struct Restore(Option<Costs>);
        impl Drop for Restore {
            fn drop(&mut self) {
                CURRENT.with(|c| c.set(self.0));
            }
        }
        let restore = Restore(CURRENT.with(|c| c.replace(Some(Costs::default()))));
        let out = run();
        let costs = CURRENT.with(|c| c.get().unwrap());
        drop(restore);
        (out, costs)
    }

    #[test]
    fn measurements_are_scoped_and_do_not_count_other_threads() {
        let (_, costs) = measure(|| {
            drop(time(Stage::Lift));
            let (_, inner) = measure(|| drop(time(Stage::Land)));
            assert_eq!((inner.up, inner.down), (0, 1));
            std::thread::spawn(|| drop(time(Stage::Transfer))).join().unwrap();
        });
        assert_eq!((costs.decoded_down, costs.up, costs.down), (0, 1, 0));
    }
}
