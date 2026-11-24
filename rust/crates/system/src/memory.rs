//! What glibc keeps after frames are freed; both do nothing without glibc.

/// Caps glibc's per-thread memory pools; with the decoder's threads it made dozens that keep freed frames. True when glibc took it.
pub fn limit_malloc_arenas(count: i32) -> bool {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        // SAFETY: mallopt only sets a tuning value
        unsafe { libc::mallopt(libc::M_ARENA_MAX, count) == 1 }
    }
    #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
    {
        let _ = count;
        false
    }
}

/// Hands memory glibc kept after frames were freed back to the system.
pub fn release_freed_memory() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    {
        glibc::THROTTLE.note(glibc::now_ms());
        // SAFETY: malloc_trim only returns free memory
        unsafe {
            libc::malloc_trim(0);
        }
    }
}

/// For a pause or a window going away: releases freed memory from a thread of its own after a short wait, at most once in a few seconds.
/// Never blocks the caller, so it is safe next to the frame path. Does nothing without glibc.
pub fn release_freed_memory_soon() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    glibc::release_soon();
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
mod glibc {
    use std::sync::atomic::{AtomicU64, Ordering::SeqCst};

    /// The least time between two releases that come from a pause or a hidden window.
    const MIN_GAP_MS: u64 = 5_000;

    /// How long the release waits, so frames still in flight at the pause or hide are freed first.
    const SETTLE_MS: u64 = 1_500;

    /// When memory was last released (milliseconds since the first call), so releases cannot follow each other closely.
    pub(super) struct Throttle(AtomicU64);

    impl Throttle {
        const NEVER: u64 = u64::MAX;

        pub(super) const fn new() -> Throttle {
            Throttle(AtomicU64::new(Self::NEVER))
        }

        pub(super) fn note(&self, now: u64) {
            self.0.store(now, SeqCst);
        }

        /// Whether a release may go now; true also notes it, so only one caller gets it.
        pub(super) fn allow(&self, now: u64, gap: u64) -> bool {
            let last = self.0.load(SeqCst);
            if last != Self::NEVER && now.saturating_sub(last) < gap {
                return false;
            }
            self.0.compare_exchange(last, now, SeqCst, SeqCst).is_ok()
        }
    }

    pub(super) static THROTTLE: Throttle = Throttle::new();

    pub(super) fn now_ms() -> u64 {
        static START: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
        START.get_or_init(std::time::Instant::now).elapsed().as_millis() as u64
    }

    pub(super) fn release_soon() {
        if !THROTTLE.allow(now_ms(), MIN_GAP_MS) {
            return;
        }
        let _ = std::thread::Builder::new().name("memory-release".into()).spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(SETTLE_MS));
            // SAFETY: malloc_trim only returns free memory
            unsafe {
                libc::malloc_trim(0);
            }
        });
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn glibc_takes_the_arena_cap_and_trimming_is_harmless() {
        let taken = super::limit_malloc_arenas(4);
        assert_eq!(taken, cfg!(all(target_os = "linux", target_env = "gnu")));
        let kept: Vec<Vec<u8>> = (0..64).map(|_| vec![1u8; 1 << 16]).collect();
        drop(kept);
        super::release_freed_memory();
        super::release_freed_memory_soon();
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn releases_that_follow_each_other_closely_are_let_through_once() {
        let t = super::glibc::Throttle::new();
        assert!(t.allow(1_000, 5_000), "the first one goes");
        assert!(!t.allow(1_001, 5_000), "one right after is held back");
        assert!(!t.allow(5_999, 5_000), "so is one inside the gap");
        assert!(t.allow(6_000, 5_000), "one after the gap goes");
        t.note(7_000);
        assert!(!t.allow(8_000, 5_000), "a release that ran on its own counts too");
    }
}
